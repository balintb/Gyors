import AppKit
import Carbon

enum HotkeyModifier {
    case command, option, control, shift

    var carbon: UInt32 {
        switch self {
        case .command: return UInt32(cmdKey)
        case .option:  return UInt32(optionKey)
        case .control: return UInt32(controlKey)
        case .shift:   return UInt32(shiftKey)
        }
    }
}

final class Hotkey {
    private var ref: EventHotKeyRef?
    /// Carbon event-handler installed by `InstallEventHandler`.
    /// Stored so `deinit` can unregister it via `RemoveEventHandler`
    /// - without that pairing, unretained `Unmanaged` opaque pointer
    /// we passed in `userData` becomes dangling if `Hotkey` is ever
    /// deallocated while handler is still attached, which would be
    /// a use-after-free the moment next hotkey event fires. Today
    /// `Hotkey` is app-lifetime and this can't actually happen, but
    /// pairing is textbook contract and costs nothing
    private var eventHandler: EventHandlerRef?
    private let onTrigger: () -> Void

    init(onTrigger: @escaping () -> Void) {
        self.onTrigger = onTrigger
    }

    /// Returns true on successful registration
    @discardableResult
    func register(keyCode: Int, modifiers: [HotkeyModifier]) -> Bool {
        let hotKeyID = EventHotKeyID(signature: UInt32(0x4759_5253), id: 1) // 'GYRS'
        let mask = modifiers.reduce(UInt32(0)) { $0 | $1.carbon }

        var spec = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )

        let selfRef = Unmanaged.passUnretained(self).toOpaque()
        var handlerRef: EventHandlerRef?
        InstallEventHandler(
            GetApplicationEventTarget(),
            { (_, _, userData) -> OSStatus in
                guard let userData = userData else { return OSStatus(eventNotHandledErr) }
                let me = Unmanaged<Hotkey>.fromOpaque(userData).takeUnretainedValue()
                me.onTrigger()
                return noErr
            },
            1,
            &spec,
            selfRef,
            &handlerRef
        )
        self.eventHandler = handlerRef

        let status = RegisterEventHotKey(
            UInt32(keyCode),
            mask,
            hotKeyID,
            GetApplicationEventTarget(),
            0,
            &ref
        )
        if status != noErr {
            NSLog("gyors: RegisterEventHotKey failed with status \(status)")
            return false
        }
        return true
    }

    deinit {
        if let ref = ref {
            UnregisterEventHotKey(ref)
        }
        if let handler = eventHandler {
            // Pair InstallEventHandler call. Without this,
            // application's event-target chain still holds our
            // unretained `Unmanaged<Hotkey>` user data after `self`
            // deallocates
            RemoveEventHandler(handler)
        }
    }
}
