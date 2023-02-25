module github.com/nicois/shorts

go 1.19

replace github.com/nicois/cache => ../cache

replace github.com/nicois/pyast => ../pyast

replace github.com/nicois/file => ../file

replace github.com/nicois/git => ../git

require (
	github.com/nicois/file v0.0.0-20230309073744-e6bf63959c2a
	github.com/nicois/git v0.0.0-20230228004916-5e651dd241ec
	github.com/nicois/pyast v0.0.0-20230309075558-80ab981ab0c1
	github.com/sirupsen/logrus v1.9.0
)

require (
	github.com/alecthomas/participle/v2 v2.0.0-beta.5 // indirect
	github.com/fsnotify/fsnotify v1.6.0 // indirect
	github.com/nicois/cache v0.0.0-20230309075418-3aae7a3eee00 // indirect
	golang.org/x/sync v0.1.0 // indirect
	golang.org/x/sys v0.0.0-20220908164124-27713097b956 // indirect
)
