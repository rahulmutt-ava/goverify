// Package annot is the phase-6 annotation-language corpus module.
package annot

//goverify:requires n >= 1
func Positive(n int) int { return n }

func CallsPositiveBad() int {
	return Positive(0) // want: contract
}

func CallsPositiveOK() int {
	return Positive(2)
}
