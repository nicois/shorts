package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"

	"github.com/nicois/file"
	"github.com/nicois/git"
	"github.com/nicois/pyast"
)

var version = "dev"

// rootFlags allows repeatable -root flags.
type rootFlags []string

func (r *rootFlags) String() string { return strings.Join(*r, ",") }
func (r *rootFlags) Set(value string) error {
	abs, err := filepath.Abs(value)
	if err != nil {
		return fmt.Errorf("invalid root path %q: %w", value, err)
	}
	*r = append(*r, abs)
	return nil
}

// excludeFlags allows repeatable -exclude flags.
type excludeFlags []string

func (e *excludeFlags) String() string { return strings.Join(*e, ",") }
func (e *excludeFlags) Set(value string) error {
	*e = append(*e, value)
	return nil
}

// jsonOutput represents the JSON output format.
type jsonOutput struct {
	Dependees    []string            `json:"dependees"`
	ChangedFiles []string            `json:"changed_files,omitempty"`
	Roots        []string            `json:"roots"`
	Explanations map[string][]string `json:"explanations,omitempty"`
}

func main() {
	os.Exit(run())
}

func run() int {
	g, err := git.Create(".")
	var upstream string
	if err == nil {
		upstream = g.GetDefaultUpstream()
	}

	var roots rootFlags
	var excludes excludeFlags

	flag.Var(&roots, "root", "Explicit Python source root (can be specified multiple times; bypasses auto-detection)")
	flag.Var(&excludes, "exclude", "Glob pattern to exclude from output (can be specified multiple times)")
	pythonNullSeparator := flag.Bool("0", false, `\0-separated output (for xargs)`)
	relative := flag.Bool("relative", false, "Show relative instead of absolute paths")
	verbose := flag.Bool("verbose", false, "verbose logging")
	ref := flag.String("ref", upstream, "git ref to calculate changes relative to (if no files are provided on the commandline)")
	quiet := flag.Bool("quiet", false, "be quiet; only log warnings and above")
	jsonMode := flag.Bool("json", false, "output results as JSON")
	explain := flag.Bool("explain", false, "show which input file triggered each dependee")
	showVersion := flag.Bool("version", false, "print version and exit")
	namespacePackages := flag.Bool("namespace-packages", false, "support PEP 420 namespace packages (root detection does not require __init__.py)")
	flag.Parse()

	if *showVersion {
		fmt.Println(version)
		return 0
	}

	logLevel := slog.LevelInfo
	if *quiet {
		logLevel = slog.LevelWarn
	} else if *verbose {
		logLevel = slog.LevelDebug
	}
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: logLevel}))
	slog.SetDefault(logger)

	var files file.Paths
	var changedFilesList []string
	if filenames := flag.Args(); len(filenames) > 0 {
		slog.Info("calculating dependees for specified files", "count", len(filenames))
		files = file.CreatePaths(filenames...)
	} else {
		if g == nil {
			slog.Error("no paths were provided, and you are not running this from inside a git repository")
			return 1
		}
		if *ref != "" && *ref != upstream {
			upstream = *ref
		}

		// Fix subdir behavior: resolve paths relative to git root
		gitRoot := g.GetRoot()
		cwd, err := filepath.Abs(".")
		if err != nil {
			slog.Error("failed to determine current directory", "error", err)
			return 1
		}
		if cwd != gitRoot {
			slog.Warn("running from subdirectory; changes are detected relative to git root", "cwd", cwd, "git_root", gitRoot)
		}

		slog.Info("calculating changes relative to upstream", "ref", upstream)
		files = g.GetChangedPaths(upstream)
		for f := range files {
			changedFilesList = append(changedFilesList, f)
		}
	}

	// Determine Python roots
	var pythonRoots file.Paths
	if len(roots) > 0 {
		pythonRoots = file.CreatePaths()
		for _, r := range roots {
			pythonRoots.Add(r)
		}
		slog.Debug("using explicit roots", "roots", []string(roots))
	} else if *namespacePackages {
		pythonRoots = calculateNamespaceRoots(files, g)
		slog.Debug("detected namespace package roots", "count", len(pythonRoots))
	} else {
		pythonRoots = pyast.CalculatePythonRoots(files)
	}

	ctx := context.Background()
	trees := pyast.BuildTrees(ctx, pythonRoots)

	cwd, err := filepath.Abs(".")
	if err != nil {
		slog.Error("failed to determine current directory", "error", err)
		return 1
	}

	if *explain {
		return outputExplain(trees, files, cwd, *relative, *jsonMode, excludes)
	}

	dependees, err := trees.GetDependees(files)
	if err != nil {
		slog.Error("failed to get dependees", "error", err)
		return 1
	}

	if *jsonMode {
		return outputJSON(dependees, changedFilesList, pythonRoots, cwd, *relative, excludes)
	}

	return outputText(dependees, cwd, *relative, *pythonNullSeparator, excludes)
}

// calculateNamespaceRoots finds roots without requiring __init__.py.
// Walks up from each file until hitting the git root or a directory
// that contains no .py files in its immediate children.
func calculateNamespaceRoots(paths file.Paths, g git.Git) file.Paths {
	result := file.CreatePaths()
	var gitRoot string
	if g != nil {
		gitRoot = g.GetRoot()
	}
	for path := range paths {
		if !strings.HasSuffix(path, ".py") {
			slog.Debug("skipping non-python file", "path", path)
			continue
		}
		dir := filepath.Dir(path)
		for {
			parent := filepath.Dir(dir)
			if parent == dir {
				break
			}
			if gitRoot != "" && dir == gitRoot {
				break
			}
			if !dirContainsPython(parent) {
				break
			}
			dir = parent
		}
		result.Add(dir)
	}
	return result
}

func dirContainsPython(dir string) bool {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return false
	}
	for _, e := range entries {
		if !e.IsDir() && strings.HasSuffix(e.Name(), ".py") {
			return true
		}
	}
	return false
}

func shouldExclude(path string, cwd string, excludes excludeFlags) bool {
	rel, err := filepath.Rel(cwd, path)
	if err != nil {
		rel = path
	}
	for _, pattern := range excludes {
		if matched, _ := filepath.Match(pattern, rel); matched {
			return true
		}
		if matched, _ := filepath.Match(pattern, filepath.Base(rel)); matched {
			return true
		}
		// Also match against directory components
		if strings.Contains(rel, string(filepath.Separator)) {
			parts := strings.Split(rel, string(filepath.Separator))
			for _, part := range parts {
				if matched, _ := filepath.Match(pattern, part); matched {
					return true
				}
			}
		}
	}
	return false
}

func formatPath(path, cwd string, relative bool) string {
	if relative {
		if rel, err := filepath.Rel(cwd, path); err == nil {
			return rel
		}
		slog.Warn("could not make path relative", "path", path)
	}
	return path
}

func outputText(dependees file.Paths, cwd string, relative, nullSep bool, excludes excludeFlags) int {
	for d := range dependees {
		if !file.FileExists(d) {
			continue
		}
		if shouldExclude(d, cwd, excludes) {
			continue
		}
		fmt.Print(formatPath(d, cwd, relative))
		if nullSep {
			fmt.Print("\x00")
		} else {
			fmt.Println()
		}
	}
	return 0
}

func outputJSON(dependees file.Paths, changedFiles []string, pythonRoots file.Paths, cwd string, relative bool, excludes excludeFlags) int {
	out := jsonOutput{
		Dependees:    make([]string, 0),
		ChangedFiles: changedFiles,
		Roots:        make([]string, 0),
	}
	for d := range dependees {
		if !file.FileExists(d) {
			continue
		}
		if shouldExclude(d, cwd, excludes) {
			continue
		}
		out.Dependees = append(out.Dependees, formatPath(d, cwd, relative))
	}
	for r := range pythonRoots {
		out.Roots = append(out.Roots, formatPath(r, cwd, relative))
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(out); err != nil {
		slog.Error("failed to encode JSON", "error", err)
		return 1
	}
	return 0
}

func outputExplain(trees interface{ GetDependees(file.Paths) (file.Paths, error) }, files file.Paths, cwd string, relative, jsonMode bool, excludes excludeFlags) int {
	// Call GetDependees per input file to track which input triggered each dependee
	triggeredBy := make(map[string][]string)
	for inputFile := range files {
		singleFile := file.CreatePaths(inputFile)
		deps, err := trees.GetDependees(singleFile)
		if err != nil {
			slog.Error("failed to get dependees", "file", inputFile, "error", err)
			return 1
		}
		for d := range deps {
			if !file.FileExists(d) {
				continue
			}
			if shouldExclude(d, cwd, excludes) {
				continue
			}
			triggeredBy[d] = append(triggeredBy[d], inputFile)
		}
	}

	if jsonMode {
		out := jsonOutput{
			Dependees:    make([]string, 0),
			Explanations: make(map[string][]string),
		}
		for d, triggers := range triggeredBy {
			formatted := formatPath(d, cwd, relative)
			out.Dependees = append(out.Dependees, formatted)
			formattedTriggers := make([]string, len(triggers))
			for i, t := range triggers {
				formattedTriggers[i] = formatPath(t, cwd, relative)
			}
			out.Explanations[formatted] = formattedTriggers
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		if err := enc.Encode(out); err != nil {
			slog.Error("failed to encode JSON", "error", err)
			return 1
		}
		return 0
	}

	for d, triggers := range triggeredBy {
		formattedTriggers := make([]string, len(triggers))
		for i, t := range triggers {
			formattedTriggers[i] = formatPath(t, cwd, relative)
		}
		fmt.Printf("%s  (triggered by: %s)\n", formatPath(d, cwd, relative), strings.Join(formattedTriggers, ", "))
	}
	return 0
}
