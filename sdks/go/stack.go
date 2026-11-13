package vaka

import (
	"runtime"
	"strings"
)

const sdkModule = "github.com/ErikHellman/vaka/sdks/go"

// Frame is one stack frame in Vaka's ingest format.
type Frame struct {
	Function string `json:"function,omitempty"`
	Module   string `json:"module,omitempty"`
	Filename string `json:"filename,omitempty"`
	AbsPath  string `json:"abs_path,omitempty"`
	Lineno   int    `json:"lineno,omitempty"`
	InApp    bool   `json:"in_app"`
}

// splitFunction splits a runtime function name such as
// "github.com/acme/shop/cart.(*Cart).Checkout.func1" into the package path and the rest.
func splitFunction(name string) (pkg, fn string) {
	slash := strings.LastIndex(name, "/")
	dot := strings.Index(name[slash+1:], ".")
	if dot < 0 {
		return "", name
	}
	return name[:slash+1+dot], name[slash+1+dot+1:]
}

// isStdlib reports whether a package path is in the standard library (no dot in the
// first path element).
func isStdlib(pkg string) bool {
	first, _, _ := strings.Cut(pkg, "/")
	return !strings.Contains(first, ".") && pkg != "main"
}

func (c *Client) inAppFrame(pkg, file string) bool {
	if pkg == sdkModule {
		return false
	}
	var include []string
	if c != nil {
		include = c.inApp
	}
	if len(include) > 0 {
		for _, p := range include {
			if pkg == p || strings.HasPrefix(pkg, p+"/") || (p != "" && strings.HasPrefix(pkg, p) && strings.HasSuffix(p, "/")) {
				return true
			}
		}
		return pkg == "main"
	}
	if isStdlib(pkg) {
		return false
	}
	f := strings.ReplaceAll(file, "\\", "/")
	return !strings.Contains(f, "/pkg/mod/") && !strings.Contains(f, "/vendor/")
}

// relPath turns an absolute build path into a path relative to the module root when it
// can, so Vaka can link frames to the repository: the package path below the module
// (cart in github.com/acme/shop/cart) plus the file name.
func (c *Client) relPath(pkg, file string) string {
	f := strings.ReplaceAll(file, "\\", "/")
	base := f[strings.LastIndex(f, "/")+1:]
	var module string
	if c != nil {
		for _, p := range c.inApp {
			p = strings.TrimSuffix(p, "/")
			if (pkg == p || strings.HasPrefix(pkg, p+"/")) && len(p) > len(module) {
				module = p
			}
		}
	}
	if module != "" {
		if rel := strings.TrimPrefix(strings.TrimPrefix(pkg, module), "/"); rel != "" {
			return rel + "/" + base
		}
		return base
	}
	// -trimpath builds already use module/path/file.go.
	if !strings.HasPrefix(f, "/") && !strings.Contains(f, ":") {
		return f
	}
	return base
}

// Stacktrace returns the current goroutine's stack, crash site first, skipping `skip`
// frames above the caller (0 = the caller of Stacktrace).
func (c *Client) Stacktrace(skip int) []Frame {
	pcs := make([]uintptr, 64)
	n := runtime.Callers(skip+2, pcs)
	return c.framesFromPCs(pcs[:n])
}

func (c *Client) framesFromPCs(pcs []uintptr) []Frame {
	frames := runtime.CallersFrames(pcs)
	var out []Frame
	for {
		f, more := frames.Next()
		if f.Function != "" {
			pkg, fn := splitFunction(f.Function)
			out = append(out, Frame{
				Function: fn,
				Module:   pkg,
				Filename: c.relPath(pkg, f.File),
				AbsPath:  f.File,
				Lineno:   f.Line,
				InApp:    c.inAppFrame(pkg, f.File),
			})
		}
		if !more {
			break
		}
	}
	return out
}

// trimLeading drops SDK and runtime frames at the top of a stack (the panic machinery and
// the SDK's own capture functions), so the first frame is where the error happened.
func trimLeading(frames []Frame) []Frame {
	for len(frames) > 1 {
		m := frames[0].Module
		if m == "runtime" || m == sdkModule {
			frames = frames[1:]
			continue
		}
		break
	}
	return frames
}
