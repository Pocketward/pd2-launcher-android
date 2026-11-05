package com.pd2launcher.app

import android.content.Context
import android.content.res.AssetManager
import com.github.luben.zstd.ZstdInputStreamNoFinalizer
import java.io.BufferedInputStream
import java.io.InputStream
import java.io.SequenceInputStream
import java.util.Enumeration

/**
 * The App userland archive and manifest in the APK's assets (userland/pack.sh), and the parts
 * that an APK carries or leaves out: the system-driver mode's ([SYSTEM_DRIVER], build.sh's
 * PD2_SYSTEM_DRIVER=0 leaves it out). The app unpacks the main archive and the parts it carries
 * into one tree, one version ([UserlandManifest.withPart]).
 */
object UserlandAssets {
    private const val FOLDER = "userland"
    private const val ARCHIVE = "userland.tar.zst"
    private const val MANIFEST = "manifest.json"
    /** The system-driver mode's part: the Venus ICD and the older GL stack (build-sysdrv.sh). */
    const val SYSTEM_DRIVER = "userland-system-driver"

    /** The parts this APK carries, in order. */
    private fun parts(context: Context): List<String> = listOf(SYSTEM_DRIVER).filter { carries(context, it) }

    /** Whether this APK carries the part [folder]. */
    fun carries(context: Context, folder: String): Boolean = MANIFEST in context.assets.list(folder).orEmpty()

    private fun read(context: Context, folder: String): UserlandManifest =
        UserlandManifest.parse(context.assets.open("$folder/$MANIFEST").use { it.readBytes().decodeToString() })

    /** The main manifest with the entries of every part this APK carries. */
    fun manifest(context: Context): UserlandManifest =
        parts(context).fold(read(context, FOLDER)) { manifest, part -> manifest.withPart(read(context, part)) }

    /**
     * The main archive, then each part's, as one uncompressed tar stream of archives one after the
     * other; each is opened when the one before it has ended (and closed by the stream).
     */
    fun openTar(context: Context): InputStream {
        val folders = (listOf(FOLDER) + parts(context)).iterator()
        return SequenceInputStream(
            object : Enumeration<InputStream> {
                override fun hasMoreElements() = folders.hasNext()
                override fun nextElement() = open(context, folders.next())
            },
        )
    }

    private fun open(context: Context, folder: String): InputStream {
        val compressed = BufferedInputStream(context.assets.open("$folder/$ARCHIVE", AssetManager.ACCESS_STREAMING), 1 shl 20)
        return BufferedInputStream(ZstdInputStreamNoFinalizer(compressed), 1 shl 20)
    }
}
