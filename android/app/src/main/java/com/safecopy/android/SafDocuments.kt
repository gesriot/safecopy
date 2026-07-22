package com.safecopy.android

import android.content.ContentResolver
import android.net.Uri
import android.provider.DocumentsContract
import java.io.InputStreamReader
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets

class SafDocuments(private val resolver: ContentResolver) {
    data class Info(
        val uri: Uri,
        val documentId: String,
        val name: String,
        val mimeType: String,
        val size: Long,
        val lastModified: Long,
    ) {
        val isDirectory: Boolean
            get() = mimeType == DocumentsContract.Document.MIME_TYPE_DIR
    }

    private val projection = arrayOf(
        DocumentsContract.Document.COLUMN_DOCUMENT_ID,
        DocumentsContract.Document.COLUMN_DISPLAY_NAME,
        DocumentsContract.Document.COLUMN_MIME_TYPE,
        DocumentsContract.Document.COLUMN_SIZE,
        DocumentsContract.Document.COLUMN_LAST_MODIFIED,
    )
    private val childrenCache = mutableMapOf<String, List<Info>>()
    private val parentByChild = mutableMapOf<String, Uri>()

    fun rootDocumentUri(treeUri: Uri): Uri = DocumentsContract.buildDocumentUriUsingTree(
        treeUri,
        DocumentsContract.getTreeDocumentId(treeUri),
    )

    fun info(uri: Uri): Info {
        try {
            resolver.query(uri, projection, null, null, null)?.use { cursor ->
                if (!cursor.moveToFirst()) {
                    throw SafOperationException("Документ недоступен: $uri")
                }
                return infoFromCursor(uri, cursor)
            }
        } catch (error: SafOperationException) {
            throw error
        } catch (error: Exception) {
            throw SafOperationException("Не удалось прочитать документ: $uri", error)
        }
        throw SafOperationException("Провайдер не вернул документ: $uri", fatalWithoutErrno = true)
    }

    fun children(parentUri: Uri): List<Info> {
        val cacheKey = parentUri.toString()
        childrenCache[cacheKey]?.let { return it }
        val parentId = DocumentsContract.getDocumentId(parentUri)
        val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(parentUri, parentId)
        val result = mutableListOf<Info>()
        try {
            resolver.query(childrenUri, projection, null, null, null)?.use { cursor ->
                while (cursor.moveToNext()) {
                    val id = cursor.getString(0)
                    val childUri = DocumentsContract.buildDocumentUriUsingTree(parentUri, id)
                    result += infoFromCursor(childUri, cursor)
                    parentByChild[childUri.toString()] = parentUri
                }
            } ?: throw SafOperationException(
                "Провайдер не вернул содержимое папки",
                fatalWithoutErrno = true,
            )
        } catch (error: SafOperationException) {
            throw error
        } catch (error: Exception) {
            throw SafOperationException("Не удалось прочитать содержимое папки", error)
        }
        return result
            .sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name })
            .also { childrenCache[cacheKey] = it }
    }

    fun findChild(parentUri: Uri, name: String): Info? {
        val children = children(parentUri)
        return children.firstOrNull { it.name == name }
            ?: children.firstOrNull { it.name.equals(name, ignoreCase = true) }
    }

    fun createDirectory(parentUri: Uri, name: String): Uri {
        val existing = findChild(parentUri, name)
        if (existing != null) {
            if (!existing.isDirectory) {
                throw SafOperationException("На накопителе уже есть файл «$name»")
            }
            return existing.uri
        }
        return createDocument(parentUri, DocumentsContract.Document.MIME_TYPE_DIR, name, "папку")
    }

    fun createFile(parentUri: Uri, mimeType: String, name: String): Uri {
        return createDocument(parentUri, mimeType.ifBlank { OCTET_STREAM }, name, "файл")
    }

    fun createTemporaryFile(parentUri: Uri, name: String): Uri {
        val uri = createDocument(parentUri, OCTET_STREAM, name, "временный файл")
        val actual = info(uri).name
        if (actual != name) {
            delete(uri)
            throw SafOperationException(
                "SAF-провайдер изменил имя временного файла «$name» на «$actual»",
                fatalWithoutErrno = true,
            )
        }
        return uri
    }

    fun delete(uri: Uri): Boolean {
        val parent = parentByChild[uri.toString()]
        val deleted = runCatching { DocumentsContract.deleteDocument(resolver, uri) }.getOrDefault(false)
        if (deleted && parent != null) invalidate(parent)
        parentByChild.remove(uri.toString())
        return deleted
    }

    fun rename(uri: Uri, newName: String): Uri {
        val parent = parentByChild[uri.toString()]
        val renamed = try {
            DocumentsContract.renameDocument(resolver, uri, newName)
        } catch (error: Exception) {
            throw SafOperationException("Не удалось переименовать файл в «$newName»", error)
        } ?: throw SafOperationException(
            "Провайдер отказался переименовать файл в «$newName»",
            fatalWithoutErrno = true,
        )
        if (parent != null) {
            invalidate(parent)
            parentByChild[renamed.toString()] = parent
        }
        parentByChild.remove(uri.toString())
        return renamed
    }

    fun renameExact(uri: Uri, newName: String): Uri {
        val renamed = rename(uri, newName)
        val actual = info(renamed).name
        if (actual != newName) {
            throw SafOperationException(
                "SAF-провайдер изменил итоговое имя «$newName» на «$actual»",
                fatalWithoutErrno = true,
            )
        }
        return renamed
    }

    fun ensureDirectory(rootUri: Uri, parts: List<String>): Uri {
        var current = rootUri
        for (part in parts) {
            current = createDirectory(current, part)
        }
        return current
    }

    fun resolve(rootUri: Uri, parts: List<String>): Uri? {
        var current = rootUri
        for (part in parts) {
            val child = findChild(current, part) ?: return null
            current = child.uri
        }
        return current
    }

    fun scan(
        selection: SourceSelection,
        respectGitignore: Boolean,
        skipJunk: Boolean,
        onWarning: (String) -> Unit,
        checkCancelled: () -> Unit,
    ): List<SourceEntry> {
        if (!selection.isTree) {
            val item = info(selection.uri)
            check(!item.isDirectory) { "Вместо файла выбрана папка" }
            return listOf(item.toSourceEntry(listOf(item.name)))
        }

        val root = rootDocumentUri(selection.uri)
        val files = mutableListOf<SourceEntry>()
        val rootParts = if (selection.includeRoot) listOf(selection.displayName) else emptyList()
        scanDirectory(
            directory = root,
            scanDirectoryParts = emptyList(),
            outputDirectoryParts = rootParts,
            rules = GitIgnoreRules(),
            respectGitignore = respectGitignore,
            skipJunk = skipJunk,
            onWarning = onWarning,
            output = files,
            checkCancelled = checkCancelled,
        )
        return files
    }

    private fun createDocument(
        parentUri: Uri,
        mimeType: String,
        name: String,
        kind: String,
    ): Uri {
        val created = try {
            DocumentsContract.createDocument(resolver, parentUri, mimeType, name)
        } catch (error: Exception) {
            throw SafOperationException(
                "Не удалось создать $kind «$name»",
                error,
                fatalWithoutErrno = true,
            )
        } ?: throw SafOperationException(
            "Провайдер не смог создать $kind «$name»",
            fatalWithoutErrno = true,
        )
        invalidate(parentUri)
        parentByChild[created.toString()] = parentUri
        return created
    }

    private fun invalidate(parentUri: Uri) {
        childrenCache.remove(parentUri.toString())
    }

    private fun scanDirectory(
        directory: Uri,
        scanDirectoryParts: List<String>,
        outputDirectoryParts: List<String>,
        rules: GitIgnoreRules,
        respectGitignore: Boolean,
        skipJunk: Boolean,
        onWarning: (String) -> Unit,
        output: MutableList<SourceEntry>,
        checkCancelled: () -> Unit,
    ) {
        checkCancelled()
        val children = children(directory)
        val effectiveRules = if (respectGitignore) {
            val gitignore = children.firstOrNull { !it.isDirectory && it.name == ".gitignore" }
            if (gitignore == null) rules else rules.withFile(
                scanDirectoryParts,
                readGitignore(gitignore.uri, scanDirectoryParts, onWarning),
            )
        } else {
            rules
        }

        for (child in children) {
            checkCancelled()
            val scanRelative = scanDirectoryParts + child.name
            if (skipJunk && SourceFilters.isJunk(scanRelative, child.isDirectory)) continue
            if (
                respectGitignore &&
                effectiveRules.isIgnored(scanRelative, child.isDirectory)
            ) continue

            val outputRelative = outputDirectoryParts + child.name
            if (child.isDirectory) {
                scanDirectory(
                    directory = child.uri,
                    scanDirectoryParts = scanRelative,
                    outputDirectoryParts = outputRelative,
                    rules = effectiveRules,
                    respectGitignore = respectGitignore,
                    skipJunk = skipJunk,
                    onWarning = onWarning,
                    output = output,
                    checkCancelled = checkCancelled,
                )
            } else {
                output += child.toSourceEntry(outputRelative)
            }
        }
    }

    private fun readGitignore(
        uri: Uri,
        directoryParts: List<String>,
        onWarning: (String) -> Unit,
    ): List<String> {
        val displayPath = (directoryParts + ".gitignore").joinToString("/")
        return readGitignoreBestEffort(displayPath, onWarning) {
            val input = resolver.openInputStream(uri) ?: error("провайдер не открыл файл")
            val decoder = StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
            input.use { stream ->
                InputStreamReader(stream, decoder).buffered().use { it.readLines() }
            }
        }
    }

    private fun Info.toSourceEntry(relative: List<String>) = SourceEntry(
        uri = uri,
        relativeParts = relative,
        displayName = name,
        mimeType = mimeType,
        size = size.coerceAtLeast(0),
    )

    private fun infoFromCursor(uri: Uri, cursor: android.database.Cursor): Info = Info(
        uri = uri,
        documentId = cursor.getString(0),
        name = cursor.getString(1) ?: "Без имени",
        mimeType = cursor.getString(2) ?: "application/octet-stream",
        size = if (cursor.isNull(3)) 0 else cursor.getLong(3),
        lastModified = if (cursor.isNull(4)) 0 else cursor.getLong(4),
    )

    companion object {
        private const val OCTET_STREAM = "application/octet-stream"
    }
}
