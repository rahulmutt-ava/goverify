// Package withdeps exercises dependency traversal: extracting it must
// also emit .gvir for "strings" and its transitive closure.
package withdeps

import "strings"

func Shout(s string) string { return strings.ToUpper(s) + "!" }
