//! The install script in each Installer Tome
//! (`InstallCD\InstallerFileList\InstallerFileList.xml`), read for the
//! Windows English install into a plan of four operations: extract a Tome
//! member into the game folder, add a Tome member to a game archive, store
//! the key or the owner name in a game archive, and delete a plain file of
//! the game folder.
//!
//! Only the elements these scripts use are known; any other element, and
//! any condition not listed, fails the read. What the Launcher does not
//! need is skipped on purpose: DirectX, shortcuts, registry values, the
//! uninstaller and the other disc of a disc set.

/// The read a plan is refused past.
const MAX_OPERATIONS: usize = 512;
/// Elements nested deeper than this are refused; the scripts nest six.
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Operation {
    /// Copy Tome member `from` to file `to` of the game folder; `size` when
    /// the script names an exact one.
    Extract {
        from: String,
        to: String,
        size: Option<u64>,
    },
    /// Add Tome member `from` to archive `container` as `to`.
    Add {
        container: String,
        from: String,
        to: String,
        size: Option<u64>,
    },
    /// Store the key for `product` in archive `container` as `into`.
    Key {
        container: String,
        into: String,
        product: u32,
    },
    /// Store the owner name in archive `container` as `into`.
    Owner { container: String, into: String },
    /// Delete plain file `name` of the game folder, when it is there.
    Delete { name: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScriptError {
    /// Not well-formed XML.
    Xml,
    /// An element this reader does not know.
    Element(String),
    /// A condition this reader does not know.
    Condition(String),
    /// A known element in a form this reader does not take (a missing
    /// attribute, a file outside the archive, a name with a path).
    Shape(&'static str),
    /// More than [`MAX_OPERATIONS`].
    TooMany,
    /// Nested deeper than [`MAX_DEPTH`].
    TooDeep,
}

/// The plan of `script` for a Windows English install over a Diablo II that
/// is installed (Lord of Destruction's script asks).
pub(crate) fn plan(script: &str) -> Result<Vec<Operation>, ScriptError> {
    let script = script.strip_prefix('\u{feff}').unwrap_or(script);
    let document = roxmltree::Document::parse(script).map_err(|_| ScriptError::Xml)?;
    let mut walk = Walk {
        operations: Vec::new(),
        depth: 0,
    };
    let root = document.root_element();
    if !matches!(root.tag_name().name(), "install" | "multipass_install") {
        return Err(ScriptError::Element(root.tag_name().name().to_owned()));
    }
    walk.children(root, Scope::default())?;
    Ok(walk.operations)
}

#[derive(Clone, Copy, Default)]
struct Scope<'a> {
    /// In a `<target location="user">`: the game folder.
    user: bool,
    /// In the `<archive>` of the Tome.
    archive: bool,
    /// In a `<repack_into>`: its type and container.
    into: Option<(&'a str, Option<&'a str>)>,
}

struct Walk {
    operations: Vec<Operation>,
    depth: usize,
}

impl Walk {
    fn children<'a>(
        &mut self,
        node: roxmltree::Node<'a, 'a>,
        scope: Scope<'a>,
    ) -> Result<(), ScriptError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ScriptError::TooDeep);
        }
        for child in node.children().filter(roxmltree::Node::is_element) {
            self.element(child, scope)?;
        }
        self.depth -= 1;
        Ok(())
    }

    fn push(&mut self, operation: Operation) -> Result<(), ScriptError> {
        if self.operations.len() >= MAX_OPERATIONS {
            return Err(ScriptError::TooMany);
        }
        self.operations.push(operation);
        Ok(())
    }

    fn element<'a>(
        &mut self,
        node: roxmltree::Node<'a, 'a>,
        scope: Scope<'a>,
    ) -> Result<(), ScriptError> {
        let name = node.tag_name().name();
        match name {
            "install" | "multipass_install" | "pass" | "disc" => self.children(node, scope),
            "archive" => self.children(
                node,
                Scope {
                    archive: true,
                    ..scope
                },
            ),
            "if" => {
                let holds = condition(node)?;
                let branches: Vec<_> = node
                    .children()
                    .filter(|child| {
                        child.is_element() && matches!(child.tag_name().name(), "then" | "else")
                    })
                    .collect();
                if branches.is_empty() {
                    return if holds {
                        self.children(node, scope)
                    } else {
                        Ok(())
                    };
                }
                if node.children().filter(roxmltree::Node::is_element).count() != branches.len() {
                    return Err(ScriptError::Shape(
                        "an if with both branches and statements",
                    ));
                }
                let wanted = if holds { "then" } else { "else" };
                for branch in branches {
                    if branch.tag_name().name() == wanted {
                        self.children(branch, scope)?;
                    }
                }
                Ok(())
            }
            "target" => {
                let location = node
                    .attribute("location")
                    .or_else(|| node.attribute("destination"))
                    .ok_or(ScriptError::Shape("a target without a location"))?;
                if location == "user" {
                    self.children(
                        node,
                        Scope {
                            user: true,
                            ..scope
                        },
                    )
                } else {
                    Ok(())
                }
            }
            // A copy of a game archive for the uninstaller, from the game
            // folder into itself.
            "source" => Ok(()),
            "repack_into" => {
                if !scope.user {
                    return Ok(());
                }
                if !scope.archive {
                    return Err(ScriptError::Shape("files from outside the archive"));
                }
                let kind = node
                    .attribute("type")
                    .ok_or(ScriptError::Shape("a repack_into without a type"))?;
                let container = node.attribute("container");
                match (kind, container) {
                    ("file", None) | ("mpq", Some(_)) => {}
                    _ => return Err(ScriptError::Shape("a repack_into of another kind")),
                }
                self.children(
                    node,
                    Scope {
                        into: Some((kind, container)),
                        ..scope
                    },
                )
            }
            "repack" => {
                let Some((kind, container)) = scope.into else {
                    return Err(ScriptError::Shape("a repack outside a repack_into"));
                };
                let from = node
                    .attribute("from")
                    .ok_or(ScriptError::Shape("a repack without a Tome member"))?
                    .to_owned();
                let to = plain(
                    node.attribute("to")
                        .ok_or(ScriptError::Shape("a repack without a target"))?,
                )?;
                let size = size(node.attribute("size"))?;
                self.push(match (kind, container) {
                    ("mpq", Some(container)) => Operation::Add {
                        container: plain(container)?,
                        from,
                        to,
                        size,
                    },
                    _ => Operation::Extract { from, to, size },
                })
            }
            "encrypt" => {
                let Some(("mpq", Some(container))) = scope.into else {
                    return Err(ScriptError::Shape("an encrypt outside an archive"));
                };
                let container = plain(container)?;
                let into = node
                    .attribute("into")
                    .ok_or(ScriptError::Shape("an encrypt without a member"))?
                    .to_owned();
                match node.attribute("object") {
                    Some("cdkey26") => self.push(Operation::Key {
                        container,
                        into,
                        product: node
                            .attribute("product_id")
                            .and_then(|value| value.parse().ok())
                            .ok_or(ScriptError::Shape("a key without a product"))?,
                    }),
                    Some("user") => self.push(Operation::Owner { container, into }),
                    _ => Err(ScriptError::Shape("an encrypt of another object")),
                }
            }
            "delete" => {
                if !matches!(node.attribute("type"), Some("file")) {
                    return Ok(());
                }
                let path = node
                    .attribute("path")
                    .ok_or(ScriptError::Shape("a delete without a path"))?;
                // Only plain files of the game folder; the rest (another
                // place, a variable, a folder) does not concern the Launcher.
                match path.strip_prefix("{OriginalInstallPath}").map(plain) {
                    Some(Ok(name)) => self.push(Operation::Delete { name }),
                    _ => Ok(()),
                }
            }
            "replace"
            | "create_folder"
            | "install_directx"
            | "registry_key"
            | "create_shortcut"
            | "add_remove_programs"
            | "update_uninstall_data" => Ok(()),
            other => Err(ScriptError::Element(other.to_owned())),
        }
    }
}

/// Whether the `if` holds for a Windows English install over an installed
/// Diablo II.
fn condition(node: roxmltree::Node<'_, '_>) -> Result<bool, ScriptError> {
    let (name, wanted) = match (
        node.attribute("true_condition"),
        node.attribute("false_condition"),
    ) {
        (Some(name), None) => (name, true),
        (None, Some(name)) => (name, false),
        _ => return Err(ScriptError::Shape("an if without one condition")),
    };
    let value = match name {
        "Win32" | "D2MusicInstalled" | "D2CharInstalled" | "D2MoviesInstalled" => true,
        "MacOS" | "German" | "French" | "Spanish" | "Korean" | "SimplifiedChinese"
        | "TraditionalChinese" | "Polish" | "Italian" => false,
        other => return Err(ScriptError::Condition(other.to_owned())),
    };
    Ok(value == wanted)
}

/// A name of one component: no path, no variable, no wildcard.
fn plain(name: &str) -> Result<String, ScriptError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['\\', '/', '{', '}', '*', '?', ':', '\0'])
    {
        return Err(ScriptError::Shape("a name with a path"));
    }
    Ok(name.to_owned())
}

/// An exact size, or none when the script marks it uncertain (`123?`).
fn size(value: Option<&str>) -> Result<Option<u64>, ScriptError> {
    match value {
        None => Ok(None),
        Some(value) if value.ends_with('?') => Ok(None),
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| ScriptError::Shape("a size that is not a number")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- A synthetic script in the shape of Blizzard's. -->
<install>
<if true_condition="Win32"><replace symbol="Launch" with="Game.exe" /></if>
<if true_condition="German"><then>
  <replace symbol="Name" with="Spiel" />
</then><else>
  <replace symbol="Name" with="Game" />
</else></if>
<if true_condition="Win32">
  <delete type="file" path="{OriginalInstallPath}old.dll" optional="1" />
  <delete type="file" path="{OriginalInstallPath}sub\old.dll" optional="1" />
  <delete type="recursive_folder" path="{OriginalInstallPath}support" optional="1" />
  <target location="start_menu"><delete type="recursive_folder" path="" /></target>
</if>
<if false_condition="D2MusicInstalled">
  <disc with_title="x" with_file="music.mpq" name="x">
    <target location="user"><repack_into type="file"><repack input0="music.mpq" to="music.mpq" size="1?" /></repack_into></target>
  </disc>
</if>
<target location="user"><source location="root"><repack_into type="file"><repack from="x" to="x" /></repack_into></source></target>
<disc with_title="t" with_file="Tome" name="n">
<archive origin="disc" input="#INPUT#" path="Tome">
  <if true_condition="MacOS">
    <target location="user"><repack_into type="file"><repack from="MAC\x" to="x.app" size="1" /></repack_into></target>
  </if>
  <if true_condition="Win32">
    <target location="user">
      <repack_into type="file" conflict="replace">
        <repack from="Common\data.mpq" to="data.mpq" size="1000" />
        <repack from="PC\Game.exe" to="Game.exe" size="20" />
      </repack_into>
    </target>
    <target location="user">
      <repack_into type="mpq" container="data.mpq" options="disable_compression/1">
        <repack from="PC-100\a.dll" to="a.dll" size="30" />
      </repack_into>
    </target>
    <target location="uninstall"><repack_into type="file"><repack input0="u.exe" to="u.exe" /></repack_into></target>
    <target location="user">
      <repack_into type="mpq" container="data.mpq" options="disable_compression/1">
        <encrypt object="cdkey26" into="data\global\key.wav" prompt="{P}" product_id="24" />
        <encrypt object="user" into="data\global\owner.wav" />
      </repack_into>
    </target>
  </if>
</archive>
</disc>
<target location="desktop"><create_shortcut path="Game.exe" /></target>
<target location="user"><registry_key where="user" key_name="x" value_name="y" /></target>
</install>
"##;

    #[test]
    fn a_script_becomes_its_windows_plan() {
        let extract = |from: &str, to: &str, size| Operation::Extract {
            from: from.to_owned(),
            to: to.to_owned(),
            size: Some(size),
        };
        assert_eq!(
            plan(SCRIPT).unwrap(),
            [
                Operation::Delete {
                    name: "old.dll".to_owned()
                },
                extract(r"Common\data.mpq", "data.mpq", 1000),
                extract(r"PC\Game.exe", "Game.exe", 20),
                Operation::Add {
                    container: "data.mpq".to_owned(),
                    from: r"PC-100\a.dll".to_owned(),
                    to: "a.dll".to_owned(),
                    size: Some(30),
                },
                Operation::Key {
                    container: "data.mpq".to_owned(),
                    into: r"data\global\key.wav".to_owned(),
                    product: 24,
                },
                Operation::Owner {
                    container: "data.mpq".to_owned(),
                    into: r"data\global\owner.wav".to_owned(),
                },
            ]
        );
        // With a byte-order mark too.
        assert_eq!(plan(&format!("\u{feff}{SCRIPT}")).unwrap().len(), 6);
    }

    #[test]
    fn anything_unknown_fails_the_read() {
        let wrap = |body: &str| format!("<install>{body}</install>");
        assert_eq!(
            plan(&wrap("<launch_game />")).unwrap_err(),
            ScriptError::Element("launch_game".to_owned())
        );
        assert_eq!(
            plan(&wrap(r#"<if true_condition="Linux"></if>"#)).unwrap_err(),
            ScriptError::Condition("Linux".to_owned())
        );
        assert!(matches!(
            plan(&wrap(
                r#"<archive><target location="user"><repack_into type="file"><repack from="a" to="..\x" /></repack_into></target></archive>"#
            )),
            Err(ScriptError::Shape(_))
        ));
        assert!(matches!(
            plan(&wrap(
                r#"<target location="user"><repack_into type="file"><repack from="a" to="x" /></repack_into></target>"#
            )),
            Err(ScriptError::Shape(_))
        ));
        assert!(matches!(
            plan(&wrap(
                r#"<archive><target location="user"><repack_into type="mpq" container="d.mpq"><encrypt object="usbkey26" into="k" /></repack_into></target></archive>"#
            )),
            Err(ScriptError::Shape(_))
        ));
        assert_eq!(plan("<install").unwrap_err(), ScriptError::Xml);
        assert_eq!(
            plan("<other />").unwrap_err(),
            ScriptError::Element("other".to_owned())
        );
        let many = wrap(&r#"<delete type="file" path="{OriginalInstallPath}a" />"#.repeat(600));
        assert_eq!(plan(&many).unwrap_err(), ScriptError::TooMany);
        let nested = |levels: usize| {
            wrap(&format!(
                "{}{}",
                r#"<if true_condition="Win32">"#.repeat(levels),
                "</if>".repeat(levels)
            ))
        };
        assert_eq!(plan(&nested(100)).unwrap_err(), ScriptError::TooDeep);
        assert_eq!(plan(&nested(10)).unwrap(), []);
    }
}
