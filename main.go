package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"

	"github.com/nicois/file"
	"github.com/nicois/git"
	"github.com/nicois/pyast"
	log "github.com/sirupsen/logrus"
)

// FIXME: if in subdir and change not under there,

// there will be no results

func main() {
	pythonNullSeparator := flag.Bool("0", false, `\0-separated output (for xargs)`)
	relative := flag.Bool("relative", false, "Show relative instead of absolute paths")
	verbose := flag.Bool("verbose", false, `verbose logging`)
	quiet := flag.Bool("quiet", false, `be quiet; only log warnings and above`)
	flag.Parse()
	if *quiet {
		log.SetLevel(log.WarnLevel)
	} else if *verbose {
		log.SetLevel(log.DebugLevel)
	}
	var files file.Paths
	g, _ := git.Create(".")
	if filenames := flag.Args(); len(filenames) > 0 {
		files = file.CreatePaths(filenames...)
	} else {
		if g == nil {
			log.Fatal("No paths were provided, and you are not running this from inside a git repository.")
		}
		upstream := g.GetDefaultUpstream()
		log.Infof("As no paths were provided, calculating changes relative to %v, and reporting modules which depend on them.", upstream)
		files = g.GetChangedPaths(upstream)
	}
	pythonRoots := pyast.CalculatePythonRoots(files)
	trees := pyast.BuildTrees(pythonRoots, g)
	dependees, err := trees.GetDependees(files)
	if err != nil {
		log.Fatal(err)
		os.Exit(1)
	}
	cwd, err := filepath.Abs(".")
	if err != nil {
		panic(err)
	}
	for d := range dependees {
		if !file.FileExists(d) {
			continue
		}
		if *relative {
			if rel, err := filepath.Rel(cwd, d); err != nil {
				log.Warn(err)
				fmt.Print(d)
			} else {
				fmt.Print(rel)
			}
		} else {
			fmt.Print(d)
		}
		if *pythonNullSeparator {
			fmt.Print("\x00")
		} else {
			fmt.Println("")
		}
	}
}
