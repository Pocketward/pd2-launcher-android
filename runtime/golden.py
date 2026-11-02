#!/usr/bin/env python3
"""golden.py <plain> <golden> <data> <golden.reg> <x64 exe> <x86 exe> <work>: the GOLDEN template.

<golden> becomes a copy of the PLAIN template <plain> (links as links) with the golden layer: what
the official installers create (Blizzard's Diablo II and Lord of Destruction installers, then the
Project Diablo 2 installer with the Visual C++ redist), except Blizzard's own files and the values
that name them. Nothing is run:
  - the files of <data>/files.txt: the Visual C++ files come from Microsoft's two official
    installers <x64 exe> and <x86 exe> (config.sh pins them), extracted as cabinets with cabextract
    (the Burn bundle's attached container, then the package cabinets inside it); the two cached
    installers of the Package Cache are the official files cut and changed as <data>/cached-exe.txt
    says; the small files made on the device are <data>/<name>.hex. Each file must have the size
    and the SHA-256 that files.txt names, and must be new in the template;
  - <golden.reg>'s keys and values written into the template's hives: a value replaces the value
    of the same name in a key that is there, and a key that is not there is added. The result is
    read again, and each value of <golden.reg> must be there as it is;
  - .pd2-prefix-template says "golden" (the PLAIN template says "plain").
<work> holds the extracted cabinets and is removed at the end."""
import hashlib
import os
import re
import shutil
import struct
import subprocess
import sys

KEY = re.compile(r"^\[(.*)\](?: \d+)?$")
CAB = b"MSCF\0\0\0\0"


def fail(message):
    raise SystemExit(f"golden.py: {message}")


def cabinets(exe):
    """The cabinets of a Burn bundle, in order: each MSCF header with version 1.3 and its size."""
    data = open(exe, "rb").read()
    found, at = [], data.find(CAB)
    while at >= 0:
        size = struct.unpack_from("<I", data, at + 8)[0]
        if data[at + 24:at + 26] == b"\x03\x01" and at + size <= len(data):
            found.append(data[at:at + size])
            at = data.find(CAB, at + size)
        else:
            at = data.find(CAB, at + 4)
    return found


def extract(cab, folder):
    """cabextract's output only when it fails (it warns about the bytes after a payload's cabinet)."""
    os.makedirs(folder)
    done = subprocess.run(["cabextract", "-q", "-d", folder, cab], capture_output=True, text=True)
    if done.returncode != 0:
        fail(f"cabextract {cab}: {done.stderr.strip()}")


def payloads(exe, work):
    """{payload name: path} of the bundle's cabinets."""
    out = {}
    os.makedirs(work)
    for n, cab in enumerate(cabinets(exe)):
        path = os.path.join(work, f"container-{n}.cab")
        open(path, "wb").write(cab)
        extract(path, os.path.join(work, f"container-{n}"))
        for name in os.listdir(os.path.join(work, f"container-{n}")):
            out.setdefault(name, os.path.join(work, f"container-{n}", name))
    return out


def patched(exe, patch, arch):
    """The cached installer: the first bytes of the official one, with the patch's bytes."""
    data, block = None, None
    for line in open(patch):
        words = line.split()
        if not words or line.startswith("#"):
            continue
        if words[0] == "file":
            block = words[1]
            if block == arch:
                data = bytearray(open(exe, "rb").read()[:int(words[2])])
        elif block == arch:
            data[int(words[0], 16)] = int(words[1], 16)
    if data is None:
        fail(f"no patch for {arch}")
    return bytes(data)


def source(spec, data_dir, exes, works):
    kind, _, rest = spec.partition(":")
    if kind in exes:
        payload, _, name = rest.partition(":")
        path = works[kind][payload]
        if name:
            # A file of a payload that is a cabinet: that cabinet extracted once.
            folder = os.path.join(os.path.dirname(os.path.dirname(path)), f"payload-{payload}")
            if not os.path.isdir(folder):
                extract(path, folder)
            path = os.path.join(folder, name)
        return open(path, "rb").read()
    if kind == "patch":
        return patched(exes[rest], os.path.join(data_dir, "cached-exe.txt"), rest)
    if kind == "data":
        return bytes.fromhex(open(os.path.join(data_dir, rest + ".hex")).read())
    fail(f"unknown source {spec}")


def blocks(text):
    """A hive's text as (head, [(key, lines)]): each key with its lines up to the next key."""
    head, keys = [], []
    for line in text.split("\n"):
        match = KEY.match(line)
        if match:
            keys.append((match.group(1), [line]))
        elif keys:
            keys[-1][1].append(line)
        else:
            head.append(line)
    return head, keys


def value_name(line):
    """The name of the value a line starts, in lower case; None for any other line."""
    if line.startswith("@="):
        return "@"
    if not line.startswith('"'):
        return None
    i = 1
    while i < len(line):
        if line[i] == "\\":
            i += 2
            continue
        if line[i] == '"':
            return line[1:i].lower() if line[i + 1:i + 2] == "=" else None
        i += 1
    return None


def values(lines):
    """{name: the value's lines} of a key's lines (a value goes on while its line ends with \\)."""
    out, name = {}, None
    for line in lines[1:]:
        if name is not None and out[name][-1].endswith("\\"):
            out[name].append(line)
            continue
        name = value_name(line)
        if name is not None:
            out[name] = [line]
    return out


def golden_hives(text):
    """{hive file: [(key, lines)]} of golden.reg, by its ";; @hive <file>" lines."""
    parts, current = {}, None
    for line in text.split("\n"):
        if line.startswith(";; @hive "):
            current = line.split()[2]
            parts[current] = []
        elif current is not None:
            parts[current].append(line)
    return {hive: blocks("\n".join(lines))[1] for hive, lines in parts.items()}


def merge(text, keys):
    """The hive text with the golden keys written in (see the module)."""
    head, target = blocks(text.rstrip("\n"))
    # A value has no empty line: the empty lines are the ones between the keys.
    target = [(key, [line for line in lines if line != ""]) for key, lines in target]
    where = {key.lower(): n for n, (key, _) in enumerate(target)}
    for key, lines in keys:
        lines = [line for line in lines if line != ""]
        if key.lower() not in where:
            where[key.lower()] = len(target)
            target.append((key, lines))
            continue
        own = target[where[key.lower()]][1]
        for name, value in values(lines).items():
            mine = values(own).get(name)
            if mine is None:
                own.extend(value)
            else:
                start = own.index(mine[0])
                own[start:start + len(mine)] = value
    body = "\n\n".join("\n".join(lines) for _, lines in target)
    return "\n".join(head).rstrip("\n") + "\n\n" + body + "\n"


def check(text, keys, hive):
    _, merged = blocks(text)
    have = {key.lower(): values(lines) for key, lines in merged}
    for key, lines in keys:
        if key.lower() not in have:
            fail(f"{hive}: the key {key} is not there after the merge")
        for name, value in values(lines).items():
            if have[key.lower()].get(name) != value:
                fail(f"{hive}: {key} {name} is not the golden value after the merge")


def main():
    plain, golden, data_dir, golden_reg, x64, x86, work = sys.argv[1:8]
    shutil.rmtree(golden, ignore_errors=True)
    shutil.rmtree(work, ignore_errors=True)
    shutil.copytree(plain, golden, symlinks=True)
    exes = {"x64": x64, "x86": x86}
    works = {arch: payloads(exe, os.path.join(work, arch)) for arch, exe in exes.items()}
    count = size = 0
    for line in open(os.path.join(data_dir, "files.txt")):
        if line.startswith("#") or not line.strip():
            continue
        spec, length, sha256, path = line.rstrip("\n").split("\t")
        content = source(spec, data_dir, exes, works)
        if len(content) != int(length) or hashlib.sha256(content).hexdigest() != sha256:
            fail(f"{path} from {spec} is not the file of files.txt")
        target = os.path.join(golden, path)
        if os.path.lexists(target):
            fail(f"{path} is in the PLAIN template already")
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as out:
            out.write(content)
        os.chmod(target, 0o644)
        count += 1
        size += len(content)
    for hive, keys in golden_hives(open(golden_reg, encoding="utf-8").read()).items():
        path = os.path.join(golden, hive)
        merged = merge(open(path, encoding="utf-8").read(), keys)
        check(merged, keys, hive)
        with open(path, "w", encoding="utf-8") as out:
            out.write(merged)
        print(f"golden.py: {hive}: {len(keys)} keys written in")
    with open(os.path.join(golden, ".pd2-prefix-template"), "w") as out:
        out.write("golden\n")
    shutil.rmtree(work)
    print(f"golden.py: {count} files, {size / 1e6:.1f} MB")


if __name__ == "__main__":
    main()
