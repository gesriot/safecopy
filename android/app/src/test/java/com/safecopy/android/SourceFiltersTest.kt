package com.safecopy.android

import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SourceFiltersTest {
    @Test
    fun unreadableGitignoreWarnsAndContributesNoRules() {
        val warnings = mutableListOf<String>()

        val lines = readGitignoreBestEffort("nested/.gitignore", warnings::add) {
            throw IOException("битый поток")
        }

        assertTrue(lines.isEmpty())
        assertEquals(
            listOf(
                "Не удалось прочитать nested/.gitignore; " +
                    "правила из него пропущены: битый поток",
            ),
            warnings,
        )
    }

    @Test
    fun gitignoreSupportsAnchoringGlobsAndNegation() {
        val rules = GitIgnoreRules().withFile(
            emptyList(),
            listOf(
                "target/",
                "*.log",
                "/root-only.txt",
                "docs/*",
                "!/docs/keep/",
                "!/docs/keep/guide.md",
                "com[1-9]",
            ),
        )

        assertTrue(rules.isIgnored(listOf("target"), isDirectory = true))
        assertTrue(rules.isIgnored(listOf("nested", "target"), isDirectory = true))
        assertTrue(rules.isIgnored(listOf("nested", "debug.log"), isDirectory = false))
        assertTrue(rules.isIgnored(listOf("root-only.txt"), isDirectory = false))
        assertFalse(rules.isIgnored(listOf("nested", "root-only.txt"), isDirectory = false))
        assertTrue(rules.isIgnored(listOf("docs", "drop"), isDirectory = true))
        assertFalse(rules.isIgnored(listOf("docs", "keep"), isDirectory = true))
        assertFalse(rules.isIgnored(listOf("docs", "keep", "guide.md"), isDirectory = false))
        assertTrue(rules.isIgnored(listOf("com7"), isDirectory = false))
        assertFalse(rules.isIgnored(listOf("com0"), isDirectory = false))
    }

    @Test
    fun nestedGitignoreOverridesParentAndGlobstarMatchesZeroDirectories() {
        val parent = GitIgnoreRules().withFile(
            emptyList(),
            listOf("*.tmp", "artifacts/**/generated-?.bin"),
        )
        val nested = parent.withFile(
            listOf("module"),
            listOf("!keep.tmp", "local/"),
        )

        assertTrue(nested.isIgnored(listOf("module", "drop.tmp"), isDirectory = false))
        assertFalse(nested.isIgnored(listOf("module", "keep.tmp"), isDirectory = false))
        assertTrue(nested.isIgnored(listOf("module", "local"), isDirectory = true))
        assertTrue(
            nested.isIgnored(listOf("artifacts", "generated-a.bin"), isDirectory = false),
        )
        assertTrue(
            nested.isIgnored(
                listOf("artifacts", "deep", "tree", "generated-z.bin"),
                isDirectory = false,
            ),
        )
    }

    @Test
    fun escapedMarkersAndTrailingSpacesRemainLiteral() {
        val rules = GitIgnoreRules().withFile(
            emptyList(),
            listOf("# comment", "\\#notes", "\\!important", "name\\ ", "ignored   "),
        )

        assertTrue(rules.isIgnored(listOf("#notes"), isDirectory = false))
        assertTrue(rules.isIgnored(listOf("!important"), isDirectory = false))
        assertTrue(rules.isIgnored(listOf("name "), isDirectory = false))
        assertTrue(rules.isIgnored(listOf("ignored"), isDirectory = false))
    }

    @Test
    fun junkFilterCoversDesktopRulesAndObservedAndroidArtifacts() {
        for (directory in listOf(
            "__pycache__",
            ".nuitka-cache",
            ".build-venv",
            "module.egg-info",
            "node_modules",
            "target",
            ".gradle",
            ".kotlin",
            ".cxx",
            ".externalNativeBuild",
            ".idea",
            ".codex",
            "cmake-build-debug",
        )) {
            assertTrue(directory, SourceFilters.isJunk(listOf(directory), isDirectory = true))
        }
        assertTrue(SourceFilters.isJunk(listOf("src-tauri", "gen"), isDirectory = true))
        assertTrue(SourceFilters.isJunk(listOf("Thumbs.db"), isDirectory = false))
        assertTrue(SourceFilters.isJunk(listOf("module.pyc"), isDirectory = false))
        assertTrue(SourceFilters.isJunk(listOf("app.apk.idsig"), isDirectory = false))
        assertTrue(SourceFilters.isJunk(listOf(".trashed-123-old.apk"), isDirectory = false))
        assertFalse(SourceFilters.isJunk(listOf("dist"), isDirectory = false))
        assertFalse(SourceFilters.isJunk(listOf("src", "output"), isDirectory = true))
        assertFalse(SourceFilters.isJunk(listOf(".git"), isDirectory = true))
    }
}
