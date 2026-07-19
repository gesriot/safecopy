package com.safecopy.android

import android.content.Context
import android.net.Uri
import android.util.Base64
import java.io.BufferedWriter
import java.io.File
import java.io.FileOutputStream
import java.io.OutputStreamWriter
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.MessageDigest

class ManifestStore(private val context: Context) {
    data class Snapshot(
        val treeUri: Uri,
        val records: List<ManifestRecord>,
    )

    private val preferences = context.getSharedPreferences("manifest_index", Context.MODE_PRIVATE)
    private val directory = File(context.filesDir, "manifests")

    @Synchronized
    fun save(volumeKey: String, treeUri: Uri, records: List<ManifestRecord>) {
        directory.mkdirs()
        val fileName = "${stableId(volumeKey + treeUri)}.scm"
        val target = File(directory, fileName)
        val temp = File(directory, "$fileName.tmp")
        FileOutputStream(temp).use { output ->
            val writer = BufferedWriter(OutputStreamWriter(output, Charsets.UTF_8))
            writer.appendLine("SAFECOPY-SHA256-1")
            writer.appendLine(encode(treeUri.toString()))
            for (record in records) {
                val path = record.relativeParts.joinToString("/")
                writer.append(record.sha256)
                    .append('\t')
                    .append(record.size.toString())
                    .append('\t')
                    .append(encode(path))
                    .appendLine()
            }
            writer.flush()
            output.fd.sync()
        }
        try {
            Files.move(
                temp.toPath(),
                target.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING,
            )
        } catch (_: AtomicMoveNotSupportedException) {
            Files.move(temp.toPath(), target.toPath(), StandardCopyOption.REPLACE_EXISTING)
        }
        check(preferences.edit().putString("manifest_$volumeKey", fileName).commit()) {
            "Не удалось обновить индекс внутреннего манифеста"
        }
    }

    @Synchronized
    fun load(volumeKey: String): Snapshot? {
        val fileName = preferences.getString("manifest_$volumeKey", null) ?: return null
        val source = File(directory, fileName)
        if (!source.isFile) return null
        val lines = source.readLines()
        if (lines.size < 2 || lines[0] != "SAFECOPY-SHA256-1") return null
        val treeUri = Uri.parse(decode(lines[1]))
        val records = lines.drop(2).mapNotNull { line ->
            val fields = line.split('\t', limit = 3)
            if (fields.size != 3) return@mapNotNull null
            val path = decode(fields[2]).split('/').filter(String::isNotEmpty)
            ManifestRecord(path, fields[0], fields[1].toLongOrNull() ?: 0)
        }
        return Snapshot(treeUri, records)
    }

    fun hasManifest(volumeKey: String): Boolean = load(volumeKey)?.records?.isNotEmpty() == true

    private fun stableId(value: String): String = MessageDigest.getInstance("SHA-256")
        .digest(value.toByteArray())
        .take(12)
        .joinToString("") { "%02x".format(it) }

    private fun encode(value: String): String = Base64.encodeToString(
        value.toByteArray(Charsets.UTF_8),
        Base64.NO_WRAP or Base64.URL_SAFE,
    )

    private fun decode(value: String): String = String(
        Base64.decode(value, Base64.NO_WRAP or Base64.URL_SAFE),
        Charsets.UTF_8,
    )
}
