package main

import (
	"path/filepath"
	"testing"

	"github.com/nicois/file"
)

func TestShouldExclude(t *testing.T) {
	cwd := "/home/user/project"
	tests := []struct {
		name     string
		path     string
		excludes excludeFlags
		want     bool
	}{
		{
			name:     "no excludes",
			path:     "/home/user/project/foo.py",
			excludes: nil,
			want:     false,
		},
		{
			name:     "matching basename glob",
			path:     "/home/user/project/test_foo.py",
			excludes: excludeFlags{"test_*"},
			want:     true,
		},
		{
			name:     "matching directory component",
			path:     "/home/user/project/venv/lib/foo.py",
			excludes: excludeFlags{"venv"},
			want:     true,
		},
		{
			name:     "no match",
			path:     "/home/user/project/myapp/views.py",
			excludes: excludeFlags{"venv", "test_*"},
			want:     false,
		},
		{
			name:     "matching relative path",
			path:     "/home/user/project/foo.py",
			excludes: excludeFlags{"foo.py"},
			want:     true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := shouldExclude(tt.path, cwd, tt.excludes)
			if got != tt.want {
				t.Errorf("shouldExclude(%q, %q, %v) = %v, want %v", tt.path, cwd, tt.excludes, got, tt.want)
			}
		})
	}
}

func TestFormatPath(t *testing.T) {
	cwd := "/home/user/project"
	tests := []struct {
		name     string
		path     string
		relative bool
		want     string
	}{
		{
			name:     "absolute mode",
			path:     "/home/user/project/foo.py",
			relative: false,
			want:     "/home/user/project/foo.py",
		},
		{
			name:     "relative mode",
			path:     "/home/user/project/myapp/foo.py",
			relative: true,
			want:     "myapp/foo.py",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := formatPath(tt.path, cwd, tt.relative)
			if got != tt.want {
				t.Errorf("formatPath(%q, %q, %v) = %q, want %q", tt.path, cwd, tt.relative, got, tt.want)
			}
		})
	}
}

func TestDirContainsPython(t *testing.T) {
	testdata, err := filepath.Abs("testdata/simple")
	if err != nil {
		t.Fatal(err)
	}
	if !dirContainsPython(filepath.Join(testdata, "myapp")) {
		t.Error("expected myapp to contain Python files")
	}
	if dirContainsPython(testdata) {
		t.Error("expected testdata/simple to not directly contain Python files")
	}
}

func TestCalculateNamespaceRoots(t *testing.T) {
	testdata, err := filepath.Abs("testdata/simple")
	if err != nil {
		t.Fatal(err)
	}
	utilsPath := filepath.Join(testdata, "myapp", "utils.py")
	paths := file.CreatePaths(utilsPath)

	roots := calculateNamespaceRoots(paths, nil)
	if len(roots) != 1 {
		t.Fatalf("expected 1 root, got %d", len(roots))
	}
	// The root should be the myapp directory (since testdata/simple has no .py files)
	expectedRoot := filepath.Join(testdata, "myapp")
	if _, ok := roots[expectedRoot]; !ok {
		t.Errorf("expected root %q, got %v", expectedRoot, roots)
	}
}

func TestRootFlags(t *testing.T) {
	var r rootFlags
	if err := r.Set("/tmp/test"); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(r) != 1 {
		t.Fatalf("expected 1 root, got %d", len(r))
	}
	if r[0] != "/tmp/test" {
		t.Errorf("expected /tmp/test, got %s", r[0])
	}
}

func TestExcludeFlags(t *testing.T) {
	var e excludeFlags
	if err := e.Set("venv"); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if err := e.Set("*.pyc"); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(e) != 2 {
		t.Fatalf("expected 2 excludes, got %d", len(e))
	}
	if e.String() != "venv,*.pyc" {
		t.Errorf("unexpected String(): %s", e.String())
	}
}
