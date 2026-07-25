package annot

//goverify:ensures ret >= 1
func One() int { return 1 }

// Zero's ensures is FALSE: pins the unverified-annotation warning at
// this pragma's line (bespoke assertion — pragma lines can't carry
// want-pins).
//goverify:ensures ret >= 1
func Zero() int { return 0 }

// Named results resolve by declared name (extractor result_names).
//goverify:ensures err == nil ==> ret >= 0
func Checked(n int) (ret int, err error) {
	if n < 0 {
		return -1, errAnnot
	}
	return n, nil
}

var errAnnot = &annotError{}

type annotError struct{}

func (*annotError) Error() string { return "annot" }
