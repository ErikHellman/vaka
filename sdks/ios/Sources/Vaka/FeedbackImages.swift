#if canImport(ImageIO) && canImport(CoreGraphics)
import Foundation
import ImageIO
import CoreGraphics
#if canImport(UIKit)
import UIKit
#endif
#if canImport(AppKit) && !targetEnvironment(macCatalyst)
import AppKit
#endif

extension Vaka.Attachment {
    /// Converts any image the platform can read (HEIC, PNG, JPEG, ...) to a JPEG within the upload
    /// limits (4096 px per side, 5 MB), with orientation applied and metadata removed.
    public static func image(_ data: Data, maxPixels: Int = 2048) -> Vaka.Attachment? {
        guard let cg = FeedbackImages.decode(data, maxPixels: maxPixels) else { return nil }
        return FeedbackImages.attachment(cg)
    }
}

enum FeedbackImages {
    static func decode(_ data: Data, maxPixels: Int) -> CGImage? {
        guard let src = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let opts: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: min(maxPixels, Vaka.Attachment.maxPixels),
        ]
        return CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary)
    }

    static func jpeg(_ image: CGImage, quality: CGFloat) -> Data? {
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out as CFMutableData, "public.jpeg" as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(dest, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }

    static func scaled(_ image: CGImage, maxSide: Int) -> CGImage? {
        let side = max(image.width, image.height)
        guard side > maxSide else { return image }
        let f = CGFloat(maxSide) / CGFloat(side)
        let w = max(1, Int(CGFloat(image.width) * f)), h = max(1, Int(CGFloat(image.height) * f))
        guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return ctx.makeImage()
    }

    /// JPEG within 4096 px and 5 MB, lowering quality and then size until it fits.
    static func attachment(_ image: CGImage) -> Vaka.Attachment? {
        guard var img = scaled(image, maxSide: Vaka.Attachment.maxPixels) else { return nil }
        for _ in 0..<3 {
            for q: CGFloat in [0.7, 0.5, 0.3] {
                if let d = jpeg(img, quality: q), d.count <= Vaka.Attachment.maxSize { return .jpeg(d) }
            }
            guard let smaller = scaled(img, maxSide: max(img.width, img.height) / 2) else { return nil }
            img = smaller
        }
        return nil
    }

    /// Fill `rects` (top-left origin, in pixels) with black.
    static func masked(_ image: CGImage, rects: [CGRect]) -> CGImage? {
        guard !rects.isEmpty else { return image }
        let w = image.width, h = image.height
        guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return nil }
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        ctx.setFillColor(CGColor(gray: 0, alpha: 1))
        for r in rects {
            ctx.fill(CGRect(x: r.minX, y: CGFloat(h) - r.maxY, width: r.width, height: r.height))
        }
        return ctx.makeImage()
    }
}

#if canImport(SwiftUI)
import SwiftUI

/// Frames (global SwiftUI coordinates) of views marked with `.vakaMask()`. Main thread only.
enum MaskRegistry {
    static var frames: [UUID: CGRect] = [:]
}

struct VakaMaskModifier: ViewModifier {
    @State private var id = UUID()

    func body(content: Content) -> some View {
        content
            .background(GeometryReader { g in
                Color.clear
                    .onAppear { MaskRegistry.frames[id] = g.frame(in: .global) }
                    .onChange(of: g.frame(in: .global)) { MaskRegistry.frames[id] = $0 }
            })
            .onDisappear { MaskRegistry.frames[id] = nil }
    }
}

extension View {
    /// Black out this view in feedback screenshots (account numbers, personal data, ...).
    /// Secure text fields are blacked out automatically.
    public func vakaMask() -> some View {
        modifier(VakaMaskModifier())
    }
}
#endif

// MARK: masking flag on UIKit / AppKit views

private var maskKey: UInt8 = 0

#if canImport(UIKit)
extension UIView {
    /// Black out this view in feedback screenshots. Secure text fields are blacked out automatically.
    public var vakaMask: Bool {
        get { (objc_getAssociatedObject(self, &maskKey) as? Bool) ?? false }
        set { objc_setAssociatedObject(self, &maskKey, newValue, .OBJC_ASSOCIATION_RETAIN_NONATOMIC) }
    }
}
#elseif canImport(AppKit)
extension NSView {
    /// Black out this view in feedback screenshots. Secure text fields are blacked out automatically.
    public var vakaMask: Bool {
        get { (objc_getAssociatedObject(self, &maskKey) as? Bool) ?? false }
        set { objc_setAssociatedObject(self, &maskKey, newValue, .OBJC_ASSOCIATION_RETAIN_NONATOMIC) }
    }
}
#endif

// MARK: screenshots

extension Vaka {
    #if canImport(UIKit)
    /// A JPEG of the app's key window, with secure text fields and `vakaMask` views blacked out.
    /// Nil when there is no window. The feedback form calls this before it opens.
    @MainActor
    @available(iOSApplicationExtension, unavailable)
    @available(tvOSApplicationExtension, unavailable)
    @available(macCatalystApplicationExtension, unavailable)
    public static func screenshot() -> Attachment? {
        guard let window = FeedbackPresenter.keyWindow() else { return nil }
        let bounds = window.bounds
        let side = max(bounds.width, bounds.height)
        guard side > 0 else { return nil }
        var rects: [CGRect] = []
        collectMasks(window, in: window, into: &rects)
        rects += MaskRegistry.frames.values.map { window.convert($0, from: nil) }
        let format = UIGraphicsImageRendererFormat()
        format.scale = min(window.traitCollection.displayScale, 2, CGFloat(Attachment.maxPixels) / side)
        format.opaque = true
        let image = UIGraphicsImageRenderer(bounds: bounds, format: format).image { ctx in
            window.drawHierarchy(in: bounds, afterScreenUpdates: false)
            UIColor.black.setFill()
            for r in rects { ctx.fill(r) }
        }
        return image.cgImage.flatMap(FeedbackImages.attachment)
    }

    @MainActor
    private static func collectMasks(_ view: UIView, in window: UIWindow, into rects: inout [CGRect]) {
        if view.isHidden || view.alpha < 0.01 { return }
        if view.vakaMask || (view as? UITextField)?.isSecureTextEntry == true {
            rects.append(view.convert(view.bounds, to: window))
            return
        }
        for v in view.subviews { collectMasks(v, in: window, into: &rects) }
    }
    #elseif canImport(AppKit)
    /// A JPEG of the app's key window, with secure text fields and `vakaMask` views blacked out.
    /// Nil when there is no window. The feedback form calls this before it opens.
    @MainActor
    public static func screenshot() -> Attachment? {
        guard let window = NSApp.keyWindow ?? NSApp.mainWindow, let root = window.contentView else { return nil }
        let bounds = root.bounds
        guard bounds.width > 0, bounds.height > 0, let rep = root.bitmapImageRepForCachingDisplay(in: bounds) else { return nil }
        root.cacheDisplay(in: bounds, to: rep)
        guard let cg = rep.cgImage else { return nil }
        let sx = CGFloat(cg.width) / bounds.width, sy = CGFloat(cg.height) / bounds.height
        // Mask rects in top-left points of the content view.
        var rects: [CGRect] = []
        collectMasks(root, root: root, into: &rects)
        rects += MaskRegistry.frames.values
        let pixels = rects.map { CGRect(x: $0.minX * sx, y: $0.minY * sy, width: $0.width * sx, height: $0.height * sy) }
        return FeedbackImages.masked(cg, rects: pixels).flatMap(FeedbackImages.attachment)
    }

    @MainActor
    private static func collectMasks(_ view: NSView, root: NSView, into rects: inout [CGRect]) {
        if view.isHidden || view.alphaValue < 0.01 { return }
        if view.vakaMask || view is NSSecureTextField {
            var r = root.convert(view.bounds, from: view)
            if !root.isFlipped { r.origin.y = root.bounds.height - r.maxY }
            rects.append(r)
            return
        }
        for v in view.subviews { collectMasks(v, root: root, into: &rects) }
    }
    #endif
}
#endif
