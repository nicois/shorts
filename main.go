package main

import (
	"fmt"
	"sort"

	"github.com/nicois/pytestw/file"
	"github.com/nicois/pytestw/pyast"
)

type Alpha []string

func (a Alpha) Len() int           { return len(a) }
func (a Alpha) Swap(i, j int)      { a[i], a[j] = a[j], a[i] }
func (a Alpha) Less(i, j int) bool { return a[i] < a[j] }

// TODO: make generic when F37
func Listify(m file.Paths) []string {
	result := make([]string, len(m))
	i := 0
	for k := range m {
		result[i] = k
		i++
	}
	sort.Sort(Alpha(result))
	return result
}

func main() {
	// log.SetLevel(log.DebugLevel)
	tree := pyast.Build(".")
	for d := range tree.GetDependees(file.CreatePaths("/home/nick.farrell/git/aiven-core/aiven/prune/base.py")) {
		fmt.Println(d)
	}
}
