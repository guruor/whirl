// wh-rotate - put a random image from any configured source on the desktop, then exit.
//
// Design rule: the process lifetime is one rotation. Nothing survives it, so the
// memory floor is the cost of one rotation, not the size of the feature set.
// Sources are configuration data (a kind plus parameters), not code plugins.
//
// Platforms: macOS (cgo + AppKit), Windows (Win32), Linux (GNOME/KDE/Wayland).
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"log"
	"math/rand"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// ---------------------------------------------------------------- config

type Source struct {
	Kind       string `json:"kind"`                 // wallhaven | wikimedia | local
	Query      string `json:"query,omitempty"`      // wallhaven: api query string
	Collection string `json:"collection,omitempty"` // wallhaven: user/id
	Dir        string `json:"dir,omitempty"`        // local: directory to read
	Weight     int    `json:"weight,omitempty"`     // relative chance of being picked
}

type Config struct {
	Sources   []Source `json:"sources"`
	MinWidth  int      `json:"min_width"`
	MinHeight int      `json:"min_height"`
	CacheDir  string   `json:"cache_dir"`
	Keep      int      `json:"keep"`
	AllSpaces bool     `json:"all_spaces"`
	LogFile   string   `json:"log_file"`
}

func defaultConfig() Config {
	home, _ := os.UserHomeDir()
	return Config{
		Sources: []Source{{
			Kind:   "wallhaven",
			Query:  "sorting=random&categories=111&purity=100&ratios=16x9",
			Weight: 3,
		}},
		MinWidth:  2560,
		MinHeight: 1440,
		CacheDir:  filepath.Join(home, "Pictures", "Wallhaven"),
		Keep:      40,
		AllSpaces: true,
		LogFile:   filepath.Join(home, "Library", "Logs", "wh-rotate.log"),
	}
}

func configPath() string {
	if p := os.Getenv("WH_ROTATE_CONFIG"); p != "" {
		return p
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".config", "wh-rotate", "config.json")
}

func loadConfig() (Config, error) {
	p := configPath()
	b, err := os.ReadFile(p)
	if err != nil {
		cfg := defaultConfig()
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			return cfg, err
		}
		out, _ := json.MarshalIndent(cfg, "", "  ")
		return cfg, os.WriteFile(p, append(out, '\n'), 0o644)
	}
	cfg := defaultConfig()
	if err := json.Unmarshal(b, &cfg); err != nil {
		return Config{}, fmt.Errorf("bad config %s: %w", p, err)
	}
	return cfg, nil
}

// ---------------------------------------------------------------- sources

type candidate struct {
	ID  string
	URL string // http(s) or a local path
	W   int
	H   int
}

func weight(s Source) int {
	if s.Weight <= 0 {
		return 1
	}
	return s.Weight
}

// pickSource chooses by weight so one source can outrank another in the rotation.
func pickSource(srcs []Source, only string) (Source, error) {
	if len(srcs) == 0 {
		return Source{}, fmt.Errorf("no sources configured")
	}
	if only != "" {
		for _, s := range srcs {
			if s.Kind == only {
				return s, nil
			}
		}
		return Source{}, fmt.Errorf("no configured source of kind %q", only)
	}
	total := 0
	for _, s := range srcs {
		total += weight(s)
	}
	n := rand.Intn(total)
	for _, s := range srcs {
		if n < weight(s) {
			return s, nil
		}
		n -= weight(s)
	}
	return srcs[len(srcs)-1], nil
}

func candidates(src Source, cfg Config, hc *http.Client) ([]candidate, error) {
	switch src.Kind {
	case "wallhaven":
		return wallhavenCandidates(src, cfg, hc)
	case "wikimedia":
		return wikimediaCandidates(cfg, hc)
	case "local":
		return localCandidates(src, cfg)
	}
	return nil, fmt.Errorf("unknown source kind %q", src.Kind)
}

type whResp struct {
	Data []struct {
		ID         string `json:"id"`
		Path       string `json:"path"`
		DimensionX int    `json:"dimension_x"`
		DimensionY int    `json:"dimension_y"`
	} `json:"data"`
}

func wallhavenCandidates(src Source, cfg Config, hc *http.Client) ([]candidate, error) {
	api := "https://wallhaven.cc/api/v1/search?" + src.Query
	if src.Collection != "" {
		// the collections endpoint ignores atleast/resolutions, so filter locally
		api = "https://wallhaven.cc/api/v1/collections/" + src.Collection + "?purity=100"
	}
	resp, err := hc.Get(api)
	if err != nil {
		return nil, fmt.Errorf("wallhaven: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("wallhaven: %s", resp.Status)
	}
	var r whResp
	if err := json.NewDecoder(resp.Body).Decode(&r); err != nil {
		return nil, fmt.Errorf("wallhaven decode: %w", err)
	}
	var out []candidate
	for _, d := range r.Data {
		if d.DimensionX >= cfg.MinWidth && d.DimensionY >= cfg.MinHeight && d.Path != "" {
			out = append(out, candidate{ID: d.ID, URL: d.Path, W: d.DimensionX, H: d.DimensionY})
		}
	}
	return out, nil
}

// wikimedia asks Commons for its own 2560px rendering, so we never download or
// decode a 40 MP original just to scale it down: resolution is negotiated at
// fetch time instead of composited locally.
func wikimediaCandidates(cfg Config, hc *http.Client) ([]candidate, error) {
	api := "https://commons.wikimedia.org/w/api.php?action=query&format=json" +
		"&generator=random&grnnamespace=6&grnlimit=40" +
		"&prop=imageinfo&iiprop=url%7Csize%7Cmime&iiurlwidth=" + fmt.Sprint(cfg.MinWidth)
	req, err := http.NewRequest("GET", api, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", "wh-rotate/1.0 (personal wallpaper rotator)")
	resp, err := hc.Do(req)
	if err != nil {
		return nil, fmt.Errorf("wikimedia: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("wikimedia: %s", resp.Status)
	}
	var r struct {
		Query struct {
			Pages map[string]struct {
				ImageInfo []struct {
					Mime     string `json:"mime"`
					Width    int    `json:"width"`
					Height   int    `json:"height"`
					ThumbURL string `json:"thumburl"`
				} `json:"imageinfo"`
			} `json:"pages"`
		} `json:"query"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&r); err != nil {
		return nil, fmt.Errorf("wikimedia decode: %w", err)
	}
	var out []candidate
	for id, p := range r.Query.Pages {
		if len(p.ImageInfo) == 0 {
			continue
		}
		ii := p.ImageInfo[0]
		if ii.Mime != "image/jpeg" && ii.Mime != "image/png" {
			continue
		}
		if ii.Width < cfg.MinWidth || ii.Height < cfg.MinHeight || ii.ThumbURL == "" {
			continue
		}
		out = append(out, candidate{ID: "commons-" + id, URL: ii.ThumbURL, W: ii.Width, H: ii.Height})
	}
	return out, nil
}

func localCandidates(src Source, cfg Config) ([]candidate, error) {
	if src.Dir == "" {
		return nil, fmt.Errorf("local source needs a dir")
	}
	entries, err := os.ReadDir(src.Dir)
	if err != nil {
		return nil, fmt.Errorf("local: %w", err)
	}
	var out []candidate
	for _, e := range entries {
		if e.IsDir() {
			continue
		}
		switch strings.ToLower(filepath.Ext(e.Name())) {
		case ".jpg", ".jpeg", ".png", ".heic", ".webp":
			id := strings.TrimSuffix(e.Name(), filepath.Ext(e.Name()))
			out = append(out, candidate{ID: "local-" + id, URL: filepath.Join(src.Dir, e.Name())})
		}
	}
	if len(out) == 0 {
		return nil, fmt.Errorf("local: no images in %s", src.Dir)
	}
	return out, nil
}

// uaTransport stamps a User-Agent on every request. Wikimedia's CDN rejects
// requests without one, and several provider APIs gate on it too.
type uaTransport struct{ rt http.RoundTripper }

func (t uaTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	r.Header.Set("User-Agent", "wh-rotate/1.0 (personal wallpaper rotator; contact: local user)")
	return t.rt.RoundTrip(r)
}

func newClient() *http.Client {
	return &http.Client{
		Timeout:   2 * time.Minute,
		Transport: uaTransport{rt: http.DefaultTransport},
	}
}

// ---------------------------------------------------------------- download / cache

func download(hc *http.Client, c candidate, dir string) (string, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	ext := filepath.Ext(strings.Split(c.URL, "?")[0])
	if ext == "" || len(ext) > 5 {
		ext = ".jpg"
	}
	dest := filepath.Join(dir, "wh-"+c.ID+ext)
	if st, err := os.Stat(dest); err == nil && st.Size() > 0 {
		return dest, nil // cached from an earlier rotation
	}
	tmp := dest + ".part"

	var rc io.ReadCloser
	if strings.HasPrefix(c.URL, "http://") || strings.HasPrefix(c.URL, "https://") {
		resp, err := hc.Get(c.URL)
		if err != nil {
			return "", err
		}
		if resp.StatusCode != http.StatusOK {
			resp.Body.Close()
			return "", fmt.Errorf("download %s: %s", c.URL, resp.Status)
		}
		rc = resp.Body
	} else {
		f, err := os.Open(c.URL)
		if err != nil {
			return "", err
		}
		rc = f
	}
	defer rc.Close()

	f, err := os.Create(tmp)
	if err != nil {
		return "", err
	}
	if _, err := io.Copy(f, rc); err != nil {
		f.Close()
		os.Remove(tmp)
		return "", err
	}
	if err := f.Close(); err != nil {
		return "", err
	}
	return dest, os.Rename(tmp, dest)
}

// prune keeps the newest `keep` files; the cache is the only state we keep.
func prune(dir string, keep int) int {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return 0
	}
	type f struct {
		path string
		mod  time.Time
	}
	var files []f
	for _, e := range entries {
		if e.IsDir() || !strings.HasPrefix(e.Name(), "wh-") {
			continue
		}
		if info, err := e.Info(); err == nil {
			files = append(files, f{filepath.Join(dir, e.Name()), info.ModTime()})
		}
	}
	if len(files) <= keep {
		return 0
	}
	sort.Slice(files, func(i, j int) bool { return files[i].mod.After(files[j].mod) })
	removed := 0
	for _, x := range files[keep:] {
		if os.Remove(x.path) == nil {
			removed++
		}
	}
	return removed
}

func logLine(path, msg string) {
	if path == "" {
		return
	}
	_ = os.MkdirAll(filepath.Dir(path), 0o755)
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	fmt.Fprintf(f, "%s %s\n", time.Now().Format("2006-01-02 15:04:05"), msg)
}

// ---------------------------------------------------------------- main

func main() {
	dry := flag.Bool("dry-run", false, "pick and download, do not touch the wallpaper")
	showCfg := flag.Bool("show-config", false, "print the config path and exit")
	only := flag.String("source", "", "force one source kind (wallhaven|wikimedia|local)")
	setPath := flag.String("set", "", "set this file directly, no fetch (used by the daemon for history/prev)")
	flag.Parse()

	cfg, err := loadConfig()
	if err != nil {
		log.Fatalf("config: %v", err)
	}
	if *showCfg {
		fmt.Println(configPath())
		return
	}
	if *setPath != "" {
		if err := platformSet(*setPath, cfg.AllSpaces); err != nil {
			logLine(cfg.LogFile, "ERROR set: "+err.Error())
			log.Fatalf("set wallpaper: %v", err)
		}
		logLine(cfg.LogFile, "set path="+*setPath)
		fmt.Printf("SET %s source=history id=manual\n", *setPath)
		return
	}

	src, err := pickSource(cfg.Sources, *only)
	if err != nil {
		log.Fatalf("%v", err)
	}
	hc := newClient()

	cands, err := candidates(src, cfg, hc)
	if err != nil {
		logLine(cfg.LogFile, "ERROR "+err.Error())
		log.Fatalf("%v", err)
	}
	if len(cands) == 0 {
		err := fmt.Errorf("no candidate >= %dx%d from %s", cfg.MinWidth, cfg.MinHeight, src.Kind)
		logLine(cfg.LogFile, "ERROR "+err.Error())
		log.Fatalf("%v", err)
	}
	pick := cands[rand.Intn(len(cands))]
	dest, err := download(hc, pick, cfg.CacheDir)
	if err != nil {
		logLine(cfg.LogFile, "ERROR "+err.Error())
		log.Fatalf("%v", err)
	}
	if *dry {
		prune(cfg.CacheDir, cfg.Keep)
		fmt.Printf("WOULD-SET %s source=%s id=%s candidates=%d\n", dest, src.Kind, pick.ID, len(cands))
		return
	}
	if err := platformSet(dest, cfg.AllSpaces); err != nil {
		logLine(cfg.LogFile, "ERROR set: "+err.Error())
		log.Fatalf("set wallpaper: %v", err)
	}
	removed := prune(cfg.CacheDir, cfg.Keep)
	logLine(cfg.LogFile, fmt.Sprintf("set source=%s id=%s candidates=%d pruned=%d",
		src.Kind, pick.ID, len(cands), removed))
	fmt.Printf("SET %s source=%s id=%s candidates=%d\n", dest, src.Kind, pick.ID, len(cands))
}
