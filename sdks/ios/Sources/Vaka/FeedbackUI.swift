#if canImport(SwiftUI) && canImport(ImageIO)
import Foundation
import SwiftUI
import ImageIO
#if canImport(PhotosUI) && !os(tvOS)
import PhotosUI
#endif
#if canImport(UIKit)
import UIKit
#endif
#if canImport(AppKit) && !targetEnvironment(macCatalyst)
import AppKit
#endif

extension Vaka {
    /// Texts in the feedback form. Set your own to localize it.
    public struct FeedbackStrings {
        public var title = "Send feedback"
        public var messagePlaceholder = "What happened, or what would you like to see?"
        public var emailPlaceholder = "Email (optional)"
        public var screenshots = "Screenshots"
        public var addImage = "Add image"
        public var removeImage = "Remove image"
        public var send = "Send"
        public var cancel = "Cancel"
        public var thanks = "Thanks for your feedback!"
        public var crashTitle = "Sorry, the app crashed"
        public var crashMessage = "The app closed unexpectedly last time. What were you doing just before?"
        public var hangTitle = "Sorry for the wait"
        public var hangMessage = "The app stopped responding for a moment. What were you doing?"
        public var rateLimited = "You've sent a lot of feedback. Please try again later."
        public var failed = "Your feedback could not be sent."

        public init() {}
    }

    public struct FeedbackOptions {
        /// Attach a screenshot of the app (the user can remove it before sending).
        public var screenshot: Bool
        /// Show the optional email field.
        public var askEmail: Bool
        /// Prefill the email field, e.g. with the signed-in user's address.
        public var email: String?
        /// After a crash, or when the app recovers from a hang, ask the user what happened. The answer is
        /// linked to that crash's issue. Takes effect through `Vaka.installFeedback(_:)`.
        public var crashPrompt: Bool
        /// Ask about hangs too (when `crashPrompt` is on). At most one prompt is shown per app run.
        public var hangPrompt: Bool
        /// Tags added to every feedback item sent from the form.
        public var tags: [String: String]
        public var strings: FeedbackStrings

        public init(screenshot: Bool = true, askEmail: Bool = true, email: String? = nil, crashPrompt: Bool = false,
                    hangPrompt: Bool = true, tags: [String: String] = [:], strings: FeedbackStrings = FeedbackStrings()) {
            self.screenshot = screenshot
            self.askEmail = askEmail
            self.email = email
            self.crashPrompt = crashPrompt
            self.hangPrompt = hangPrompt
            self.tags = tags
            self.strings = strings
        }
    }

    private static let feedbackLock = NSLock()
    private static var _feedbackOptions = FeedbackOptions()

    /// Options used by `showFeedback()`, `VakaFeedbackButton` and `.vakaFeedback(isPresented:)`.
    public static var feedbackOptions: FeedbackOptions {
        get { feedbackLock.lock(); defer { feedbackLock.unlock() }; return _feedbackOptions }
        set { feedbackLock.lock(); _feedbackOptions = newValue; feedbackLock.unlock() }
    }

    /// Set the feedback form's options and, with `crashPrompt`, ask the user about crashes and hangs.
    /// Call after `Vaka.start`. Not needed for `showFeedback()` with the default options.
    @available(iOSApplicationExtension, unavailable)
    @available(tvOSApplicationExtension, unavailable)
    @available(macCatalystApplicationExtension, unavailable)
    public static func installFeedback(_ options: FeedbackOptions = FeedbackOptions()) {
        feedbackOptions = options
        guard options.crashPrompt else { return }
        onCrashOrHang { report in
            let opts = feedbackOptions
            guard opts.crashPrompt, report.kind == .crash || opts.hangPrompt else { return }
            Task { @MainActor in FeedbackPresenter.presentCrashPrompt(report) }
        }
    }

    /// Open the feedback form over the current screen, with a screenshot of it.
    /// `eventId` links the feedback to an error (see `Vaka.lastEventId`).
    @MainActor
    @available(iOSApplicationExtension, unavailable)
    @available(tvOSApplicationExtension, unavailable)
    @available(macCatalystApplicationExtension, unavailable)
    public static func showFeedback(eventId: String? = nil, options: FeedbackOptions? = nil,
                                    onFinish: ((FeedbackResult?) -> Void)? = nil) {
        let opts = options ?? feedbackOptions
        let shot = opts.screenshot ? screenshot() : nil
        FeedbackPresenter.present(FeedbackForm(mode: .general(eventId: eventId), screenshot: shot, options: opts), onFinish: onFinish)
    }
}

// MARK: form model

@MainActor
final class FeedbackFormModel: ObservableObject {
    struct Image: Identifiable {
        let id = UUID()
        let attachment: Vaka.Attachment
        let preview: CGImage?
    }

    enum Mode {
        case general(eventId: String?)
        case crash(Vaka.CrashReport)
    }

    let mode: Mode
    let options: Vaka.FeedbackOptions
    @Published var message = ""
    @Published var email: String
    @Published var images: [Image] = []
    @Published var sending = false
    @Published var sent = false
    @Published var error: String?

    init(mode: Mode, screenshot: Vaka.Attachment?, options: Vaka.FeedbackOptions) {
        self.mode = mode
        self.options = options
        email = options.email ?? ""
        if let s = screenshot { add(s) }
    }

    var title: String {
        switch mode {
        case .general: return options.strings.title
        case let .crash(r): return r.kind == .crash ? options.strings.crashTitle : options.strings.hangTitle
        }
    }

    var header: String? {
        switch mode {
        case .general: return nil
        case let .crash(r): return r.kind == .crash ? options.strings.crashMessage : options.strings.hangMessage
        }
    }

    var canSend: Bool { !sending && !sent && !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    var canAddImage: Bool { images.count < Vaka.Attachment.maxCount }

    func add(_ a: Vaka.Attachment) {
        guard canAddImage else { return }
        images.append(Image(attachment: a, preview: FeedbackImages.decode(a.data, maxPixels: 240)))
    }

    func remove(_ id: UUID) {
        images.removeAll { $0.id == id }
    }

    func send(onDone: @escaping (Vaka.FeedbackResult) -> Void) {
        guard canSend else { return }
        var f = Vaka.Feedback(message: String(message.prefix(FeedbackSender.maxMessage)),
                              email: options.askEmail ? email : nil, tags: options.tags,
                              attachments: images.map(\.attachment))
        switch mode {
        case let .general(eventId):
            f.source = .widget
            f.eventId = eventId
        case let .crash(r):
            f.source = .crashPrompt
            f.eventId = r.eventId
        }
        sending = true
        error = nil
        Task { @MainActor in
            do {
                let r = try await Vaka.captureFeedback(f)
                sending = false
                sent = true
                try? await Task.sleep(nanoseconds: 1_200_000_000)
                onDone(r)
            } catch {
                sending = false
                switch error as? Vaka.FeedbackError {
                case .rateLimited?: self.error = options.strings.rateLimited
                case .notStarted?:
                    self.error = options.strings.failed
                    print("[Vaka] feedback not sent: call Vaka.start first")
                default: self.error = options.strings.failed
                }
            }
        }
    }
}

// MARK: form view

/// The feedback form. Present it yourself, or use `Vaka.showFeedback()`, `VakaFeedbackButton` or
/// `.vakaFeedback(isPresented:)`. `onFinish` gets the result after sending, or nil on cancel.
public struct VakaFeedbackView: View {
    @StateObject private var model: FeedbackFormModel
    private let onFinish: (Vaka.FeedbackResult?) -> Void

    /// - Parameters:
    ///   - eventId: error event to link the feedback to.
    ///   - screenshot: an image to start with, e.g. `Vaka.screenshot()` taken before presenting.
    public init(eventId: String? = nil, screenshot: Vaka.Attachment? = nil, options: Vaka.FeedbackOptions = Vaka.feedbackOptions,
                onFinish: @escaping (Vaka.FeedbackResult?) -> Void) {
        self.init(form: FeedbackForm(mode: .general(eventId: eventId), screenshot: screenshot, options: options), onFinish: onFinish)
    }

    /// The crash prompt for a report from `Vaka.onCrashOrHang`.
    public init(crash: Vaka.CrashReport, screenshot: Vaka.Attachment? = nil, options: Vaka.FeedbackOptions = Vaka.feedbackOptions,
                onFinish: @escaping (Vaka.FeedbackResult?) -> Void) {
        self.init(form: FeedbackForm(mode: .crash(crash), screenshot: screenshot, options: options), onFinish: onFinish)
    }

    init(form: FeedbackForm, onFinish: @escaping (Vaka.FeedbackResult?) -> Void) {
        _model = StateObject(wrappedValue: FeedbackFormModel(mode: form.mode, screenshot: form.screenshot, options: form.options))
        self.onFinish = onFinish
    }

    public var body: some View {
        content
            .interactiveDismissDisabled(model.sending || !model.message.isEmpty)
    }

    private func send() { model.send { onFinish($0) } }

    @ViewBuilder private var thanks: some View {
        VStack(spacing: 12) {
            SwiftUI.Image(systemName: "checkmark.circle.fill").font(.system(size: 44)).foregroundColor(.green)
            Text(model.options.strings.thanks).font(.headline).multilineTextAlignment(.center)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    #if os(macOS)
    @ViewBuilder private var content: some View {
        if model.sent {
            thanks.frame(width: 440, height: 220)
        } else {
            VStack(alignment: .leading, spacing: 12) {
                Text(model.title).font(.headline)
                if let h = model.header { Text(h).foregroundColor(.secondary).fixedSize(horizontal: false, vertical: true) }
                ZStack(alignment: .topLeading) {
                    TextEditor(text: $model.message).font(.body).frame(minHeight: 120)
                    if model.message.isEmpty {
                        Text(model.options.strings.messagePlaceholder).foregroundColor(.secondary)
                            .padding(.horizontal, 5).padding(.vertical, 1).allowsHitTesting(false)
                    }
                }
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.3)))
                if model.options.askEmail {
                    TextField(model.options.strings.emailPlaceholder, text: $model.email)
                        .textFieldStyle(.roundedBorder).disableAutocorrection(true)
                }
                images
                if let e = model.error { Text(e).foregroundColor(.red).font(.callout) }
                HStack {
                    Spacer()
                    Button(model.options.strings.cancel) { onFinish(nil) }.keyboardShortcut(.cancelAction)
                    Button(model.options.strings.send, action: send).keyboardShortcut(.defaultAction).disabled(!model.canSend)
                }
            }
            .padding(20)
            .frame(width: 440)
        }
    }
    #else
    @ViewBuilder private var content: some View {
        NavigationView {
            Group {
                if model.sent {
                    thanks
                } else {
                    Form {
                        Section(footer: model.error.map { Text($0).foregroundColor(.red) }) {
                            if let h = model.header { Text(h).foregroundColor(.secondary) }
                            messageField
                            if model.options.askEmail {
                                TextField(model.options.strings.emailPlaceholder, text: $model.email)
                                    .keyboardType(.emailAddress)
                                    .textContentType(.emailAddress)
                                    .textInputAutocapitalization(.never)
                                    .disableAutocorrection(true)
                            }
                        }
                        Section(header: Text(model.options.strings.screenshots)) { images }
                    }
                }
            }
            .navigationTitle(model.title)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(model.options.strings.cancel) { onFinish(nil) }.disabled(model.sent)
                }
                ToolbarItem(placement: .confirmationAction) {
                    if model.sending {
                        ProgressView()
                    } else {
                        Button(model.options.strings.send, action: send).disabled(!model.canSend)
                    }
                }
            }
        }
        #if os(iOS)
        .navigationViewStyle(.stack)
        #endif
    }

    @ViewBuilder private var messageField: some View {
        #if os(tvOS)
        TextField(model.options.strings.messagePlaceholder, text: $model.message)
        #else
        ZStack(alignment: .topLeading) {
            TextEditor(text: $model.message).frame(minHeight: 140)
            if model.message.isEmpty {
                Text(model.options.strings.messagePlaceholder).foregroundColor(Color(UIColor.placeholderText))
                    .padding(.horizontal, 4).padding(.vertical, 8).allowsHitTesting(false)
            }
        }
        #endif
    }
    #endif

    @ViewBuilder private var images: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 12) {
                ForEach(model.images) { img in
                    ZStack(alignment: .topTrailing) {
                        Group {
                            if let p = img.preview {
                                SwiftUI.Image(decorative: p, scale: 1).resizable().aspectRatio(contentMode: .fill)
                            } else {
                                Color.secondary.opacity(0.2)
                            }
                        }
                        .frame(width: 72, height: 110)
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.secondary.opacity(0.3)))
                        Button { model.remove(img.id) } label: {
                            SwiftUI.Image(systemName: "xmark.circle.fill")
                                .font(.system(size: 22))
                                .foregroundColor(.white)
                                .background(Circle().fill(Color.black.opacity(0.6)))
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(model.options.strings.removeImage)
                        .offset(x: 6, y: -6)
                    }
                    .padding(.top, 6)
                }
                #if (os(iOS) || os(macOS)) && canImport(PhotosUI)
                if #available(iOS 16, macOS 13, *) {
                    if model.canAddImage { AddImageButton(model: model) }
                }
                #endif
            }
            .padding(.vertical, 4)
        }
    }
}

#if (os(iOS) || os(macOS)) && canImport(PhotosUI)
@available(iOS 16, macOS 13, *)
@MainActor
private struct AddImageButton: View {
    @ObservedObject var model: FeedbackFormModel
    @State private var selection: [PhotosPickerItem] = []

    var body: some View {
        PhotosPicker(selection: $selection, maxSelectionCount: max(1, Vaka.Attachment.maxCount - model.images.count), matching: .images) {
            VStack(spacing: 6) {
                SwiftUI.Image(systemName: "photo.badge.plus").font(.system(size: 22))
                Text(model.options.strings.addImage).font(.caption2).multilineTextAlignment(.center)
            }
            .frame(width: 72, height: 110)
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.secondary.opacity(0.4), style: StrokeStyle(lineWidth: 1, dash: [4])))
        }
        .buttonStyle(.plain)
        .padding(.top, 6)
        .onChange(of: selection) { items in
            guard !items.isEmpty else { return }
            selection = []
            Task { @MainActor in
                for item in items {
                    if let data = try? await item.loadTransferable(type: Data.self), let a = Vaka.Attachment.image(data) {
                        model.add(a)
                    }
                }
            }
        }
    }
}
#endif

/// What to show; built before presenting so the screenshot is taken first.
struct FeedbackForm {
    let mode: FeedbackFormModel.Mode
    let screenshot: Vaka.Attachment?
    let options: Vaka.FeedbackOptions
}

// MARK: SwiftUI entry points

@available(iOSApplicationExtension, unavailable)
@available(tvOSApplicationExtension, unavailable)
@available(macCatalystApplicationExtension, unavailable)
struct VakaFeedbackModifier: ViewModifier {
    @Binding var isPresented: Bool
    let eventId: String?
    let options: Vaka.FeedbackOptions?
    let onFinish: ((Vaka.FeedbackResult?) -> Void)?
    @State private var form: FeedbackForm?

    func body(content: Content) -> some View {
        content
            .onChange(of: isPresented) { show in
                if show && form == nil {
                    // Take the screenshot before the sheet covers the screen.
                    let opts = options ?? Vaka.feedbackOptions
                    form = FeedbackForm(mode: .general(eventId: eventId), screenshot: opts.screenshot ? Vaka.screenshot() : nil, options: opts)
                } else if !show {
                    form = nil
                }
            }
            .sheet(isPresented: Binding(get: { form != nil }, set: { if !$0 { form = nil; isPresented = false } })) {
                if let f = form {
                    VakaFeedbackView(form: f) { result in
                        form = nil
                        isPresented = false
                        onFinish?(result)
                    }
                }
            }
    }
}

extension View {
    /// Present the feedback form (with a screenshot of the current screen) while `isPresented` is true.
    @available(iOSApplicationExtension, unavailable)
    @available(tvOSApplicationExtension, unavailable)
    @available(macCatalystApplicationExtension, unavailable)
    public func vakaFeedback(isPresented: Binding<Bool>, eventId: String? = nil, options: Vaka.FeedbackOptions? = nil,
                             onFinish: ((Vaka.FeedbackResult?) -> Void)? = nil) -> some View {
        modifier(VakaFeedbackModifier(isPresented: isPresented, eventId: eventId, options: options, onFinish: onFinish))
    }
}

/// A button that opens the feedback form. Pass your own label, or use the default "Send feedback".
@available(iOSApplicationExtension, unavailable)
@available(tvOSApplicationExtension, unavailable)
@available(macCatalystApplicationExtension, unavailable)
public struct VakaFeedbackButton<Label: View>: View {
    @State private var show = false
    private let options: Vaka.FeedbackOptions?
    private let label: Label

    public init(options: Vaka.FeedbackOptions? = nil, @ViewBuilder label: () -> Label) {
        self.options = options
        self.label = label()
    }

    public var body: some View {
        Button { show = true } label: { label }
            .vakaFeedback(isPresented: $show, options: options)
    }
}

@available(iOSApplicationExtension, unavailable)
@available(tvOSApplicationExtension, unavailable)
@available(macCatalystApplicationExtension, unavailable)
extension VakaFeedbackButton where Label == SwiftUI.Label<Text, SwiftUI.Image> {
    public init(_ title: String? = nil, options: Vaka.FeedbackOptions? = nil) {
        self.init(options: options) {
            SwiftUI.Label(title ?? (options ?? Vaka.feedbackOptions).strings.title, systemImage: "bubble.left.and.exclamationmark.bubble.right")
        }
    }
}

// MARK: UIKit / AppKit presentation

#if canImport(UIKit)
/// The feedback form as a view controller, for UIKit apps that present it themselves.
/// `Vaka.showFeedback()` does this for you.
public final class VakaFeedbackViewController: UIHostingController<VakaFeedbackView> {
    /// - Parameter onFinish: called with the result after sending, or nil on cancel. The controller
    ///   dismisses itself first.
    public init(eventId: String? = nil, screenshot: Vaka.Attachment? = nil, options: Vaka.FeedbackOptions = Vaka.feedbackOptions,
                onFinish: ((Vaka.FeedbackResult?) -> Void)? = nil) {
        let box = WeakBox()
        super.init(rootView: VakaFeedbackView(eventId: eventId, screenshot: screenshot, options: options) { r in
            box.vc?.dismiss(animated: true) { onFinish?(r) }
        })
        box.vc = self
    }

    init(form: FeedbackForm, onFinish: ((Vaka.FeedbackResult?) -> Void)?) {
        let box = WeakBox()
        super.init(rootView: VakaFeedbackView(form: form) { r in
            box.vc?.dismiss(animated: true) { onFinish?(r) }
        })
        box.vc = self
    }

    @available(*, unavailable)
    required dynamic init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private final class WeakBox { weak var vc: UIViewController? }
}
#endif

@MainActor
@available(iOSApplicationExtension, unavailable)
@available(tvOSApplicationExtension, unavailable)
@available(macCatalystApplicationExtension, unavailable)
enum FeedbackPresenter {
    static var showing = false
    static var promptedThisRun = false

    #if canImport(UIKit)
    static func keyWindow() -> UIWindow? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let ordered = scenes.filter { $0.activationState == .foregroundActive } + scenes.filter { $0.activationState != .foregroundActive }
        let windows = ordered.flatMap(\.windows)
        return windows.first { $0.isKeyWindow } ?? windows.first { !$0.isHidden && $0.windowLevel == .normal }
    }

    static func topViewController() -> UIViewController? {
        var vc = keyWindow()?.rootViewController
        while let p = vc?.presentedViewController, !p.isBeingDismissed { vc = p }
        return vc
    }

    static var ready: Bool { UIApplication.shared.applicationState == .active && topViewController() != nil }

    static func present(_ form: FeedbackForm, onFinish: ((Vaka.FeedbackResult?) -> Void)?) {
        guard !showing, let top = topViewController() else { return }
        showing = true
        let vc = VakaFeedbackViewController(form: form) { r in
            showing = false
            onFinish?(r)
        }
        top.present(vc, animated: true)
    }
    #else
    static var window: NSWindow?

    static var ready: Bool { NSApp.isActive && (NSApp.keyWindow ?? NSApp.mainWindow) != nil }

    static func present(_ form: FeedbackForm, onFinish: ((Vaka.FeedbackResult?) -> Void)?) {
        guard !showing else { return }
        showing = true
        let parent = NSApp.keyWindow ?? NSApp.mainWindow
        var sheet: NSWindow?
        let host = NSHostingController(rootView: VakaFeedbackView(form: form) { r in
            if let s = sheet {
                if let p = s.sheetParent { p.endSheet(s) } else { s.close() }
            }
            window = nil
            showing = false
            onFinish?(r)
        })
        let w = NSWindow(contentViewController: host)
        w.styleMask = [.titled, .closable]
        w.title = form.options.strings.title
        w.isReleasedWhenClosed = false
        sheet = w
        window = w
        if let p = parent {
            p.beginSheet(w)
        } else {
            w.center()
            w.makeKeyAndOrderFront(nil)
        }
    }
    #endif

    /// Show the crash prompt once the app has a screen to show it on.
    static func presentCrashPrompt(_ report: Vaka.CrashReport, attempt: Int = 0) {
        guard !promptedThisRun else { return }
        guard ready, !showing else {
            // At launch the UI may not be up yet; a hang prompt is only worth it right away.
            if report.previousRun && attempt < 30 {
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 1_000_000_000)
                    presentCrashPrompt(report, attempt: attempt + 1)
                }
            }
            return
        }
        promptedThisRun = true
        let opts = Vaka.feedbackOptions
        // A screenshot only helps when the problem is on screen now (a hang this run).
        let shot = !report.previousRun && opts.screenshot ? Vaka.screenshot() : nil
        present(FeedbackForm(mode: .crash(report), screenshot: shot, options: opts), onFinish: nil)
    }
}
#endif
