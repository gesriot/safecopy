package com.safecopy.android

import android.content.Context
import android.net.Uri
import android.os.Environment
import android.os.storage.StorageManager
import android.os.storage.StorageVolume
import android.provider.DocumentsContract

class StorageLocator(private val context: Context) {
    data class MountedVolume(
        val volume: StorageVolume,
        val key: String,
        val description: String,
    )

    private val storageManager = context.getSystemService(StorageManager::class.java)
    private val preferences = context.getSharedPreferences("usb_access", Context.MODE_PRIVATE)

    fun mountedRemovable(): MountedVolume? = storageManager.storageVolumes
        .asSequence()
        .filter { it.isRemovable && !it.isPrimary && it.state == Environment.MEDIA_MOUNTED }
        .map { volume ->
            val key = volume.uuid
                ?: volume.getDescription(context)
            MountedVolume(volume, key, volume.getDescription(context))
        }
        .sortedBy { it.key }
        .firstOrNull()

    fun accessUri(mounted: MountedVolume): Uri? {
        val stored = preferences.getString("tree_${mounted.key}", null)?.let(Uri::parse)
        if (stored != null && hasWriteGrant(stored)) return stored

        val recovered = context.contentResolver.persistedUriPermissions
            .firstOrNull { permission ->
                permission.isReadPermission && permission.isWritePermission &&
                    treeVolumeId(permission.uri).equals(mounted.key, ignoreCase = true)
            }
            ?.uri
        if (recovered != null) saveAccess(mounted, recovered)
        return recovered
    }

    fun saveAccess(mounted: MountedVolume, treeUri: Uri) {
        preferences.edit().putString("tree_${mounted.key}", treeUri.toString()).apply()
    }

    fun belongsTo(mounted: MountedVolume, treeUri: Uri): Boolean {
        val treeVolume = documentVolumeId(treeUri)
        val mountedVolume = mounted.volume.uuid
            ?: accessUri(mounted)?.let(::documentVolumeId)
            ?: return false
        return treeVolume.isNotEmpty() && treeVolume.equals(mountedVolume, ignoreCase = true)
    }

    fun isMounted(volumeKey: String): Boolean = storageManager.storageVolumes.any { volume ->
        val key = volume.uuid ?: volume.getDescription(context)
        key == volumeKey && volume.state == Environment.MEDIA_MOUNTED
    }

    private fun hasWriteGrant(uri: Uri): Boolean =
        context.contentResolver.persistedUriPermissions.any {
            it.uri == uri && it.isReadPermission && it.isWritePermission
        }

    private fun treeVolumeId(uri: Uri): String = documentVolumeId(uri)

    private fun documentVolumeId(uri: Uri): String = runCatching {
        val id = if (DocumentsContract.isTreeUri(uri)) {
            DocumentsContract.getTreeDocumentId(uri)
        } else {
            DocumentsContract.getDocumentId(uri)
        }
        id.substringBefore(':')
    }.getOrDefault("")
}
