package annot

//goverify:requires q != nil
func UnknownName(p *int) bool { return p == nil }

//goverify:requires ret != 0
func ResultInRequires(p int) (ret int) { return p }

//goverify:requires p.buf != nil
func FieldSel(p *int) bool { return p != nil }

//goverify:effects locks(mu)
func Effects() {}

//goverify:pure
func Pure() {}

//goverify:frobnicate
func UnknownDirective() {}

//goverify:ignore no-such-checker
func BadIgnore() {}

//goverify:requires x >
func ParseError(x int) {}
