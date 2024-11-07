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
	g, err := git.Create(".")
	var upstream string
	if err == nil {
		upstream = g.GetDefaultUpstream()
	}
	pythonNullSeparator := flag.Bool("0", false, `\0-separated output (for xargs)`)
	relative := flag.Bool("relative", false, "Show relative instead of absolute paths")
	verbose := flag.Bool("verbose", false, `verbose logging`)
	ref := flag.String("ref", upstream, "git ref to calculate changes relative to (if no files are provided on the commandline)")
	quiet := flag.Bool("quiet", false, `be quiet; only log warnings and above`)
	flag.Parse()
	if *quiet {
		log.SetLevel(log.WarnLevel)
	} else if *verbose {
		log.SetLevel(log.DebugLevel)
	}
	var files file.Paths
	if filenames := flag.Args(); len(filenames) > 0 {
		log.Infof("calculating which modules depend on at least one of the %v specified files", len(filenames))
		files = file.CreatePaths(filenames...)
	} else {
		if g == nil {
			log.Fatal("no paths were provided, and you are not running this from inside a git repository.")
		}
		if *ref != "" && *ref != upstream {
			upstream = string(*ref)
		}
		log.Infof("calculating changes in the current branch relative to %v, and reporting modules which depend on these changes.", upstream)
		files = g.GetChangedPaths(upstream)
	}
	pythonRoots := pyast.CalculatePythonRoots(files)
	trees := pyast.BuildTrees(pythonRoots)
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
