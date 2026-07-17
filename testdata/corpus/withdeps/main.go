// Package withdeps exercises dependency traversal: extracting it must
// also emit .gvir for "strings" and its transitive closure.
package withdeps

import "strings"

func Shout(s string) string { return strings.ToUpper(s) + "!" }

type node struct{ next *node }

func chain(n *node) int {
	c := 0
	for n != nil {
		n = n.next
		c++
	}
	return c
}
