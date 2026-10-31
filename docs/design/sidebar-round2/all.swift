import AppKit
let dir = "/tmp/hk-test/variants/round2/"
let cells: [(String, String)] = [
  ("h1", "H1 Numeral cards \u{00b7} ink"), ("h2", "H2 Tinted numerals \u{00b7} graphite"), ("h3", "H3 Herdr + end number \u{00b7} vesper"), ("h4", "H4 Tinted cards \u{00b7} atom-one-dark"),
  ("h5", "H5 Herdr two-line tinted \u{00b7} vesper"), ("h6", "H6 End numeral \u{00b7} graphite"), ("h7", "H7 Tinted herdr \u{00b7} vesper"), ("h8", "H8 Numeral cards herdr \u{00b7} vesper")]
let cw: CGFloat = 300, ch: CGFloat = 1040, pad: CGFloat = 14, lab: CGFloat = 26, cols = 4
let rowsN = (cells.count + cols - 1) / cols
let W = pad + CGFloat(cols) * (cw + pad), H = pad + CGFloat(rowsN) * (ch + lab + pad)
let img = NSImage(size: NSSize(width: W, height: H))
img.lockFocus()
NSColor(white: 0.97, alpha: 1).setFill(); NSRect(x: 0, y: 0, width: W, height: H).fill()
let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: 13, weight: .semibold), .foregroundColor: NSColor(white: 0.12, alpha: 1)]
for (i, (f, name)) in cells.enumerated() {
  let c = i % cols, r = i / cols
  let x = pad + CGFloat(c) * (cw + pad)
  let top = H - pad - CGFloat(r) * (ch + lab + pad)
  (name as NSString).draw(at: NSPoint(x: x + 2, y: top - 18), withAttributes: attrs)
  guard let src = NSImage(contentsOfFile: dir + f + ".png"), let cg = src.cgImage(forProposedRect: nil, context: nil, hints: nil) else { fatalError(f) }
  var y = top - lab
  for (py, ph) in [(CGFloat(0), CGFloat(150)), (CGFloat(185), CGFloat(886))] {
    let piece = NSImage(cgImage: cg.cropping(to: CGRect(x: 0, y: py, width: cw, height: ph))!, size: NSSize(width: cw, height: ph))
    y -= ph
    piece.draw(in: NSRect(x: x, y: y, width: cw, height: ph))
    y -= 4
  }
}
img.unlockFocus()
let rep = NSBitmapImageRep(cgImage: img.cgImage(forProposedRect: nil, context: nil, hints: nil)!)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: dir + "all.png"))
print("ok", W, H)
