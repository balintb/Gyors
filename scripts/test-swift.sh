#!/usr/bin/env bash
# Swift unit tests - HotkeyBinding, Config, ViewModel

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BIN="$(mktemp -t gyors-swift-tests)"
trap "rm -f '$BIN'" EXIT

xcrun swiftc \
    -O \
    -swift-version 5 \
    -parse-as-library \
    -target arm64-apple-macos14 \
    -D CLOUD -D AI \
    -framework Foundation -framework AppKit -framework Carbon -framework Combine -framework Security \
    macos/Sources/Gyors/AiClient.swift \
    macos/Sources/Gyors/AiRouter.swift \
    macos/Sources/Gyors/AiRouterTools.swift \
    macos/Sources/Gyors/AppleFmRouter.swift \
    macos/Sources/Gyors/BusyTracker.swift \
    macos/Sources/Gyors/CaretPosition.swift \
    macos/Sources/Gyors/FoundationModelsBridge.swift \
    macos/Sources/Gyors/Candidate.swift \
    macos/Sources/Gyors/ChainInput.swift \
    macos/Sources/Gyors/Config.swift \
    macos/Sources/Gyors/MainInputField.swift \
    macos/Sources/Gyors/ConfigSchema.swift \
    macos/Sources/Gyors/CustomTheme.swift \
    macos/Sources/Gyors/MarkdownListContinuer.swift \
    macos/Sources/Gyors/Effect.swift \
    macos/Sources/Gyors/GlobalSnippetMatcher.swift \
    macos/Sources/Gyors/Hotkey.swift \
    macos/Sources/Gyors/PasteboardWatcher.swift \
    macos/Sources/Gyors/QueryBridge.swift \
    macos/Sources/Gyors/Theme.swift \
    macos/Sources/Gyors/TimerManager.swift \
    macos/Sources/Gyors/GyorsUrlHandler.swift \
    macos/Sources/Gyors/ThemeImporter.swift \
    macos/Sources/Gyors/ThemeManager.swift \
    macos/Sources/Gyors/TitleHighlight.swift \
    macos/Sources/Gyors/VisualEffectView.swift \
    macos/Sources/Gyors/ViewModel.swift \
    macos/Sources/Gyors/WindowArranger.swift \
    macos/Sources/Gyors/SyncPanelLifetime.swift \
    macos/Sources/Gyors/SyncPanelAuth.swift \
    macos/Sources/Gyors/GyorsPaths.swift \
    macos/Sources/Gyors/AiKeychain.swift \
    macos/Tests/TestRunner.swift \
    macos/Tests/TestMocks.swift \
    macos/Tests/HotkeyBindingTests.swift \
    macos/Tests/ConfigTests.swift \
    macos/Tests/ViewModelTests.swift \
    macos/Tests/ChainActionsTests.swift \
    macos/Tests/SetInputEffectTests.swift \
    macos/Tests/TabAutocompleteTests.swift \
    macos/Tests/EditorTests.swift \
    macos/Tests/MarkdownListContinuerTests.swift \
    macos/Tests/TitleHighlightTests.swift \
    macos/Tests/ActivateAtIndexTests.swift \
    macos/Tests/NoteRenameTests.swift \
    macos/Tests/UrlHandlerTests.swift \
    macos/Tests/SchemaUrlRoutesTests.swift \
    macos/Tests/QrFlowTests.swift \
    macos/Tests/AiEndpointValidationTests.swift \
    macos/Tests/AiRouterTests.swift \
    macos/Tests/AiRouterVmTests.swift \
    macos/Tests/AiRouterParsersTests.swift \
    macos/Tests/AiRouterConfigTests.swift \
    macos/Tests/AiRouterLiveTests.swift \
    macos/Tests/AppleFmLiveTests.swift \
    macos/Tests/AiPaletteTests.swift \
    macos/Tests/CaretHelperTests.swift \
    macos/Tests/EffectDecoderEditNoteTests.swift \
    macos/Tests/EffectOpenUrlAllowlistTests.swift \
    macos/Tests/NewNoteShortcutTests.swift \
    macos/Tests/ChainExplicitStateTests.swift \
    macos/Tests/NotesAutocompleteRegressionsTests.swift \
    macos/Tests/ChainStateParseTests.swift \
    macos/Tests/HistoryRecallTests.swift \
    macos/Tests/SnippetMatcherTests.swift \
    macos/Tests/SelectionTests.swift \
    macos/Tests/AiRouterToolsCatalogTests.swift \
    macos/Tests/BusyTrackerTests.swift \
    macos/Tests/SyncPanelLifetimeTests.swift \
    macos/Tests/PasteboardConcealedTests.swift \
    macos/Tests/AiKeychainTests.swift \
    macos/Tests/ThemeRemoveTests.swift \
    macos/Tests/ThemeImportSecurityTests.swift \
    macos/Tests/SyncPanelAuthResetTests.swift \
    macos/Tests/GyorsPathsSecurityTests.swift \
    -o "$BIN"
# NoteEditorView.swift / ContentView.swift are SwiftUI-only and dont
# participate in these pure-logic tests; TitleHighlight is Foundation-only
# so links fine alongside non-SwiftUI Tests harness

"$BIN"
