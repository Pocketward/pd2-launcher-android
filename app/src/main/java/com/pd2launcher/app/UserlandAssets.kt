package com.pd2launcher.app

import android.content.Context
import android.content.res.AssetManager
import com.github.luben.zstd.ZstdInputStreamNoFinalizer
import java.io.BufferedInputStream
import java.io.InputStream

/** The App userland archive and manifest in the APK's assets (userland/pack.sh). */
object UserlandAssets {
    private const val ARCHIVE = "userland/userland.tar.zst"
    private const val MANIFEST = "userland/manifest.json"

    fun manifest(context: Context): UserlandManifest =
        UserlandManifest.parse(context.assets.open(MANIFEST).use { it.readBytes().decodeToString() })

    /** The archive as an uncompressed tar stream. */
    fun openTar(context: Context): InputStream {
        val compressed = BufferedInputStream(context.assets.open(ARCHIVE, AssetManager.ACCESS_STREAMING), 1 shl 20)
        return BufferedInputStream(ZstdInputStreamNoFinalizer(compressed), 1 shl 20)
    }
}
