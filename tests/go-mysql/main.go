// This test-only launcher is copied into a temporary copy of doing-server.
// It does not import internal/config or change any production handler/service/repo.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"sync"
	"syscall"
	"time"

	"doing-server/internal/db"
	"doing-server/internal/server"

	"github.com/go-sql-driver/mysql"
	"github.com/jmoiron/sqlx"
)

const fixtureKind = "doing-tauri-isolated-go-mysql-v1"

type audit struct {
	Migrations      []int64        `json:"migrations"`
	Users           int            `json:"users"`
	Items           int            `json:"items"`
	SharedItemIDs   int            `json:"sharedItemIds"`
	ActiveSessions  int            `json:"activeSessions"`
	RevokedSessions int            `json:"revokedSessions"`
	Requests        map[string]int `json:"requests"`
}

type fixture struct {
	db       *sqlx.DB
	nonce    string
	mu       sync.Mutex
	requests map[string]int
}

// Aggregate SQL evidence only: no tokens, password hashes, usernames or task text.
func (f *fixture) audit() (audit, error) {
	a := audit{Requests: map[string]int{}}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := f.db.SelectContext(ctx, &a.Migrations, "SELECT version FROM schema_migrations ORDER BY version"); err != nil {
		return a, err
	}
	for _, q := range []struct {
		dest *int
		sql  string
	}{
		{&a.Users, "SELECT COUNT(*) FROM users"},
		{&a.Items, "SELECT COUNT(*) FROM items"},
		{&a.SharedItemIDs, "SELECT COUNT(*) FROM (SELECT id FROM items GROUP BY id HAVING COUNT(DISTINCT user_id)>1) AS shared_ids"},
		{&a.ActiveSessions, "SELECT COUNT(*) FROM refresh_sessions WHERE revoked_at IS NULL"},
		{&a.RevokedSessions, "SELECT COUNT(*) FROM refresh_sessions WHERE revoked_at IS NOT NULL"},
	} {
		if err := f.db.GetContext(ctx, q.dest, q.sql); err != nil {
			return a, err
		}
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	for k, v := range f.requests {
		a.Requests[k] = v
	}
	return a, nil
}

type statusWriter struct {
	http.ResponseWriter
	status int
}

func (w *statusWriter) WriteHeader(status int) {
	if w.status == 0 {
		w.status = status
		w.ResponseWriter.WriteHeader(status)
	}
}
func (w *statusWriter) Write(b []byte) (int, error) {
	if w.status == 0 {
		w.WriteHeader(http.StatusOK)
	}
	return w.ResponseWriter.Write(b)
}

func (f *fixture) wrap(router http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-Doing-Test-Fixture", f.nonce)
		if r.URL.Path == "/_doing_test/audit" {
			if r.Method != http.MethodGet || r.Header.Get("X-Doing-Test-Fixture") != f.nonce {
				http.NotFound(w, r)
				return
			}
			a, err := f.audit()
			if err != nil {
				http.Error(w, "fixture audit failed", http.StatusInternalServerError)
				return
			}
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(a)
			return
		}
		rw := &statusWriter{ResponseWriter: w}
		router.ServeHTTP(rw, r)
		status := rw.status
		if status == 0 {
			status = http.StatusOK
		}
		f.mu.Lock()
		f.requests[fmt.Sprintf("%s %s %d", r.Method, r.URL.Path, status)]++
		f.mu.Unlock()
	})
}

func privateJSON(path string, value any) error {
	b, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	f, err := os.OpenFile(path+".tmp", os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer f.Close()
	defer os.Remove(path + ".tmp")
	if _, err = f.Write(b); err != nil {
		return err
	}
	if err = f.Sync(); err != nil {
		return err
	}
	if err = f.Close(); err != nil {
		return err
	}
	return os.Rename(path+".tmp", path)
}

func run() error {
	root := os.Getenv("DOING_TEST_FIXTURE_ROOT")
	nonce := os.Getenv("DOING_TEST_FIXTURE_NONCE")
	secret := os.Getenv("DOING_TEST_JWT_SECRET")
	port, err := strconv.Atoi(os.Getenv("DOING_TEST_API_PORT"))
	if err != nil || port < 0 || port > 65535 || len(nonce) < 32 || len(secret) < 32 || !filepath.IsAbs(root) {
		return errors.New("missing or invalid isolated fixture configuration")
	}
	info, err := os.Lstat(root)
	if err != nil || !info.IsDir() || info.Mode().Perm()&0077 != 0 {
		return errors.New("fixture root must be a private, non-symlink directory")
	}
	marker, err := os.ReadFile(filepath.Join(root, "fixture-kind"))
	if err != nil || string(marker) != fixtureKind {
		return errors.New("refusing an unmarked fixture directory")
	}
	socket := filepath.Join(root, "mysql.sock")
	info, err = os.Lstat(socket)
	if err != nil || info.Mode()&os.ModeSocket == 0 {
		return errors.New("private MySQL socket is unavailable")
	}
	cfg := mysql.NewConfig()
	cfg.User, cfg.Net, cfg.Addr = "root", "unix", socket
	cfg.ParseTime, cfg.Loc = true, time.UTC
	cfg.Timeout, cfg.ReadTimeout, cfg.WriteTimeout = 5*time.Second, 5*time.Second, 5*time.Second
	admin, err := db.Open(cfg.FormatDSN())
	if err != nil {
		return errors.New("cannot open private MySQL socket")
	}
	defer admin.Close()
	var dataDir string
	var skipNetworking int
	if err = admin.Get(&dataDir, "SELECT @@datadir"); err != nil {
		return errors.New("cannot verify MySQL data directory")
	}
	if err = admin.Get(&skipNetworking, "SELECT @@skip_networking"); err != nil {
		return errors.New("cannot verify MySQL networking is disabled")
	}
	expected, err := filepath.EvalSymlinks(filepath.Join(root, "data"))
	actual, resolveErr := filepath.EvalSymlinks(dataDir)
	if err != nil || resolveErr != nil || expected != actual || skipNetworking != 1 {
		return errors.New("refusing a database outside the isolated socket-only fixture")
	}
	if _, err = admin.Exec("CREATE DATABASE IF NOT EXISTS doing_tauri_contract CHARACTER SET utf8mb4"); err != nil {
		return errors.New("cannot create isolated test schema")
	}
	cfg.DBName = "doing_tauri_contract"
	if err = db.Migrate(cfg.FormatDSN()); err != nil {
		return errors.New("original Go migrations failed")
	}
	database, err := db.Open(cfg.FormatDSN())
	if err != nil {
		return errors.New("cannot open isolated test schema")
	}
	defer database.Close()
	listener, err := net.Listen("tcp4", fmt.Sprintf("127.0.0.1:%d", port))
	if err != nil {
		return errors.New("cannot bind the loopback-only test API")
	}
	defer listener.Close()
	f := &fixture{db: database, nonce: nonce, requests: map[string]int{}}
	router := server.New(server.Options{DB: database, Secret: secret, Access: 3 * time.Second, Refresh: time.Hour})
	srv := &http.Server{Handler: f.wrap(router), ReadHeaderTimeout: 5 * time.Second}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	done := make(chan error, 1)
	go func() { done <- srv.Serve(listener) }()
	if err = privateJSON(filepath.Join(root, "ready.json"), map[string]any{
		"kind": fixtureKind, "baseUrl": "http://" + listener.Addr().String(),
		"nonce": nonce, "accessTtlSeconds": 3, "pid": os.Getpid(),
	}); err != nil {
		_ = srv.Close()
		return errors.New("cannot publish fixture readiness")
	}
	select {
	case err = <-done:
		if !errors.Is(err, http.ErrServerClosed) {
			return errors.New("test API stopped unexpectedly")
		}
	case <-ctx.Done():
		shutdown, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err = srv.Shutdown(shutdown); err != nil {
			return errors.New("test API did not shut down cleanly")
		}
	}
	a, err := f.audit()
	if err != nil || privateJSON(filepath.Join(root, "audit.json"), a) != nil {
		return errors.New("cannot write final aggregate SQL evidence")
	}
	return nil
}

func main() {
	if err := run(); err != nil {
		// Messages above are stage labels, never the original DSN or credentials.
		log.Print(err)
		os.Exit(1)
	}
}
