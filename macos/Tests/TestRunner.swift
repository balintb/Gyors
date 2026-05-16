import Foundation

// Globals shared by every per-topic test file. The original test runner
// kept them as locals inside `main()`; promoting to globals lets us
// split tests across files without passing closures around

var testsPassed = 0
var testsFailed = 0

/// Assert and tally. Prints a FAIL line with file:line on miss
func expect(
    _ condition: @autoclosure () -> Bool,
    _ message: String,
    file: StaticString = #file,
    line: UInt = #line
) {
    if condition() { testsPassed += 1 }
    else {
        testsFailed += 1
        print("FAIL  \(message)  (\(file):\(line))")
    }
}

/// Wrap a test block with a name. Prints the name after the block runs
/// so the failing assertions land *above* the group label in output
func runGroup(_ name: String, _ block: () -> Void) {
    block()
    print("  \(name)")
}

@main
enum TestMain {
    static func main() {
        runHotkeyBindingTests()
        runConfigTests()
        runViewModelTests()
        runChainActionsTests()
        runSetInputEffectTests()
        runTabAutocompleteTests()
        runEditorTests()
        runMarkdownListContinuerTests()
        runTitleHighlightTests()
        runActivateAtIndexTests()
        runNoteRenameTests()
        runUrlHandlerTests()
        runSchemaUrlRoutesTests()
        runQrFlowTests()
        runAiEndpointValidationTests()
        runAiRouterTests()
        runAiRouterVmTests()
        runAiRouterParsersTests()
        runAiRouterConfigTests()
        runAiRouterLiveTests()
        runAppleFmLiveTests()
        runAiPaletteTests()
        runCaretHelperTests()
        runEffectDecoderEditNoteTests()
        runEffectOpenUrlAllowlistTests()
        runNewNoteShortcutTests()
        runChainExplicitStateTests()
        runNotesAutocompleteRegressionsTests()
        runChainStateParseTests()
        runHistoryRecallTests()
        runSnippetMatcherTests()
        runSelectionTests()
        runAiRouterToolsCatalogTests()
        runBusyTrackerTests()
        MainActor.assumeIsolated {
            runSyncPanelLifetimeTests()
        }
        runPasteboardConcealedTests()
        runAiKeychainTests()
        runThemeRemoveTests()
        runThemeImportSecurityTests()
        runSyncPanelAuthResetTests()
        runGyorsPathsSecurityTests()

        print("")
        print("\(testsPassed) passed, \(testsFailed) failed")
        exit(testsFailed == 0 ? 0 : 1)
    }
}
