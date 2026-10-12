package com.pd2launcher.app

import java.io.File
import java.io.IOException
import java.nio.file.DirectoryNotEmptyException
import java.nio.file.FileVisitResult
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.SimpleFileVisitor
import java.nio.file.attribute.BasicFileAttributes

/**
 * The prefix's Windows Installer cache: Wine keeps a copy of each installed MSI (Gecko, Mono,
 * about 130 MB) there, which Windows reads only to repair or uninstall that MSI. It is an
 * environment dependency's installer file, so the app removes the copies by itself once the
 * installation is Ready, like the runtime's archive copies.
 * Nothing needs restoring: the Ready audit checks Gecko and Mono by the Launcher's receipts and
 * installed files, never through these copies.
 */
object InstallerCache {
    /** Below the Launcher's managed root. */
    const val PATH = "prefix/drive_c/windows/Installer"

    /** Folders below [PATH] searched for cached MSI files. */
    private const val DEPTH = 3

    /**
     * Removes the regular `.msi` files below [PATH] in [managedRoot], then each folder this
     * removal emptied (one it removed a file or a folder from); nothing else, so a folder that
     * was empty before stays. Links are never followed or removed: each folder from [managedRoot]
     * down to the cache is checked not to be one (`lstat`, not opened: the prefix is the app's
     * own, and nothing swaps a folder in it for a link meanwhile), and the walk below follows
     * none. Returns the files removed.
     */
    fun remove(managedRoot: File): Int {
        val cache = PATH.split('/').fold(managedRoot.toPath()) { folder, name ->
            folder.resolve(name).takeIf { Files.isDirectory(it, LinkOption.NOFOLLOW_LINKS) } ?: return 0
        }
        var removed = 0
        val emptied = HashSet<Path>()
        Files.walkFileTree(cache, emptySet(), DEPTH + 1, object : SimpleFileVisitor<Path>() {
            override fun visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult {
                if (attrs.isRegularFile && file.fileName.toString().endsWith(".msi", ignoreCase = true)) {
                    Files.delete(file)
                    removed++
                    emptied.add(file.parent)
                }
                return FileVisitResult.CONTINUE
            }

            override fun visitFileFailed(file: Path, exc: IOException): FileVisitResult = FileVisitResult.CONTINUE

            override fun postVisitDirectory(dir: Path, exc: IOException?): FileVisitResult {
                // Only a folder this removal took something from goes, once empty; the cache
                // folder itself stays.
                if (dir != cache && dir in emptied) {
                    try {
                        Files.delete(dir)
                        emptied.add(dir.parent)
                    } catch (_: DirectoryNotEmptyException) {
                    }
                }
                return FileVisitResult.CONTINUE
            }
        })
        return removed
    }
}
