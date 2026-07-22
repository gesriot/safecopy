package com.safecopy.android

internal fun readGitignoreBestEffort(
    displayPath: String,
    onWarning: (String) -> Unit,
    read: () -> List<String>,
): List<String> {
    return try {
        read()
    } catch (error: Exception) {
        var root: Throwable = error
        while (root.cause != null && root.cause !== root) root = root.cause!!
        onWarning(
            "Не удалось прочитать $displayPath; правила из него пропущены: " +
                (root.message ?: root.javaClass.simpleName),
        )
        emptyList()
    }
}

/** Filters that can be applied while the SAF source tree is being scanned. */
internal object SourceFilters {
    private val junkDirectories = setOf(
        ".agents",
        ".angular",
        ".claude",
        ".codex",
        ".cxx",
        ".dart_tool",
        ".eggs",
        ".externalnativebuild",
        ".gradle",
        ".idea",
        ".kotlin",
        ".mplconfig",
        ".next",
        ".nox",
        ".nuxt",
        ".svelte-kit",
        ".tauri",
        ".terraform",
        ".tox",
        ".turbo",
        ".venv",
        ".vscode",
        "__pycache__",
        "bower_components",
        "build",
        "cmakefiles",
        "coverage",
        "deriveddata",
        "dist",
        "htmlcov",
        "node_modules",
        "target",
        "venv",
    )

    private val junkFiles = setOf(
        ".coverage",
        ".ds_store",
        ".eslintcache",
        ".localized",
        "desktop.ini",
        "nuitka-crash-report.xml",
        "thumbs.db",
    )

    fun isJunk(relativeParts: List<String>, isDirectory: Boolean): Boolean {
        val name = relativeParts.lastOrNull()?.lowercase() ?: return false
        if (isDirectory) {
            return name in junkDirectories ||
                (name.startsWith('.') && name.endsWith("cache")) ||
                name.endsWith("-venv") ||
                name.startsWith(".pytest-tmp") ||
                name.endsWith(".egg-info") ||
                name.endsWith(".build") ||
                name.endsWith(".dist") ||
                name.startsWith("cmake-build-") ||
                relativeParts.takeLast(2).map(String::lowercase) == listOf("src-tauri", "gen")
        }

        return name in junkFiles ||
            name.startsWith(".coverage.") ||
            name.startsWith(".trashed-") ||
            name.startsWith("._") ||
            name.endsWith(".idsig") ||
            name.endsWith(".pyc") ||
            name.endsWith(".pyo")
    }
}

/**
 * Immutable stack of rules collected from .gitignore files on the path from the
 * selected root to the directory currently being scanned. Later rules win.
 */
internal class GitIgnoreRules private constructor(
    private val rules: List<GitIgnoreRule>,
) {
    constructor() : this(emptyList())

    fun withFile(basePath: List<String>, lines: List<String>): GitIgnoreRules {
        val additions = lines.mapIndexedNotNull { index, line ->
            GitIgnoreRule.parse(basePath, line, firstLine = index == 0)
        }
        return if (additions.isEmpty()) this else GitIgnoreRules(rules + additions)
    }

    fun isIgnored(path: List<String>, isDirectory: Boolean): Boolean {
        var ignored = false
        for (rule in rules) {
            if (rule.matches(path, isDirectory)) ignored = !rule.negated
        }
        return ignored
    }
}

private data class GitIgnoreRule(
    val basePath: List<String>,
    val negated: Boolean,
    val directoryOnly: Boolean,
    val pathPattern: Boolean,
    val regex: Regex,
) {
    fun matches(path: List<String>, isDirectory: Boolean): Boolean {
        if (directoryOnly && !isDirectory) return false
        if (path.size <= basePath.size) return false
        for (index in basePath.indices) {
            if (path[index] != basePath[index]) return false
        }

        val relative = path.subList(basePath.size, path.size)
        val candidate = if (pathPattern) relative.joinToString("/") else relative.last()
        return regex.matches(candidate)
    }

    companion object {
        fun parse(basePath: List<String>, rawLine: String, firstLine: Boolean): GitIgnoreRule? {
            var line = rawLine.removeSuffix("\r")
            if (firstLine) line = line.removePrefix("\uFEFF")
            line = trimUnescapedTrailingSpaces(line)
            if (line.isEmpty() || line.startsWith("#")) return null

            val negated = line.startsWith("!")
            if (negated) line = line.substring(1)
            if (line.isEmpty()) return null

            val directoryOnly = line.endsWith("/")
            if (directoryOnly) line = line.dropLast(1)
            val anchored = line.startsWith("/")
            if (anchored) line = line.substring(1)
            if (line.isEmpty()) return null

            val pathPattern = anchored || line.contains('/')
            return GitIgnoreRule(
                basePath = basePath.toList(),
                negated = negated,
                directoryOnly = directoryOnly,
                pathPattern = pathPattern,
                regex = globRegex(line),
            )
        }

        private fun trimUnescapedTrailingSpaces(value: String): String {
            var end = value.length
            while (end > 0 && value[end - 1] == ' ') {
                var backslashes = 0
                var index = end - 2
                while (index >= 0 && value[index] == '\\') {
                    backslashes += 1
                    index -= 1
                }
                if (backslashes % 2 == 1) break
                end -= 1
            }
            return value.substring(0, end)
        }

        private fun globRegex(glob: String): Regex {
            val output = StringBuilder("^")
            var index = 0
            while (index < glob.length) {
                when (val char = glob[index]) {
                    '\\' -> {
                        if (index + 1 < glob.length) {
                            appendRegexLiteral(output, glob[index + 1])
                            index += 2
                        } else {
                            appendRegexLiteral(output, char)
                            index += 1
                        }
                    }
                    '*' -> {
                        var end = index + 1
                        while (end < glob.length && glob[end] == '*') end += 1
                        val doubleStar = end - index == 2 &&
                            (index == 0 || glob[index - 1] == '/') &&
                            (end == glob.length || glob[end] == '/')
                        if (doubleStar) {
                            if (end < glob.length && glob[end] == '/') {
                                output.append("(?:.*/)?")
                                index = end + 1
                            } else {
                                output.append(".*")
                                index = end
                            }
                        } else {
                            output.append("[^/]*")
                            index = end
                        }
                    }
                    '?' -> {
                        output.append("[^/]")
                        index += 1
                    }
                    '[' -> {
                        val consumed = appendCharacterClass(output, glob, index)
                        if (consumed == 0) {
                            output.append("\\[")
                            index += 1
                        } else {
                            index += consumed
                        }
                    }
                    else -> {
                        appendRegexLiteral(output, char)
                        index += 1
                    }
                }
            }
            output.append('$')
            return Regex(output.toString())
        }

        private fun appendCharacterClass(output: StringBuilder, glob: String, start: Int): Int {
            var end = start + 1
            if (end < glob.length && (glob[end] == '!' || glob[end] == '^')) end += 1
            if (end < glob.length && glob[end] == ']') end += 1
            while (end < glob.length && glob[end] != ']') end += 1
            if (end >= glob.length) return 0

            var index = start + 1
            output.append('[')
            if (index < end && (glob[index] == '!' || glob[index] == '^')) {
                output.append('^')
                index += 1
            }
            if (index < end && glob[index] == ']') {
                output.append("\\]")
                index += 1
            }
            while (index < end) {
                when (val char = glob[index]) {
                    '\\' -> output.append("\\\\")
                    '[' -> output.append("\\[")
                    else -> output.append(char)
                }
                index += 1
            }
            output.append(']')
            return end - start + 1
        }

        private fun appendRegexLiteral(output: StringBuilder, char: Char) {
            if (char in "\\.[]{}()+-^$|") output.append('\\')
            output.append(char)
        }
    }
}
