// Go, written as a careful author would: errgroup.WithContext + SetLimit,
// context.WithTimeout per attempt. The site model and probe mirror
// ../python/site_model.py; see ../README.md.
//
//	go run . <scenario>
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"runtime"
	"sort"
	"sync"
	"sync/atomic"
	"time"

	"golang.org/x/sync/errgroup"
)

type spec struct {
	scenario                        string
	tenants, items, bound, attempts int
	wait, deadline, cancelAfter     time.Duration
}

func specOf(scenario string) (spec, error) {
	machine := runtime.NumCPU() * 64
	table := map[string][4]int{
		"throughput": {2, 10_000, machine, 3},
		"failfast":   {1, 2_000, machine, 1},
		"cancel":     {1, 2_000, machine, 1},
		"storm":      {1, 1_000, 100, 5},
		"deadline":   {1, 1, 1, 1},
	}
	row, ok := table[scenario]
	if !ok {
		return spec{}, fmt.Errorf("unknown scenario %q", scenario)
	}
	s := spec{scenario: scenario, tenants: row[0], items: row[1], bound: row[2], attempts: row[3], deadline: time.Second}
	switch scenario {
	case "throughput":
		s.wait = 5 * time.Millisecond
	case "deadline":
		s.deadline = 100 * time.Millisecond
	case "cancel":
		s.cancelAfter = 10 * time.Millisecond
	}
	return s, nil
}

var errTransient = errors.New("transient")

type site struct {
	spec                                       spec
	live, peak, finished, attempts, duplicates atomic.Int64
	mu                                         sync.Mutex
	latencies                                  []int64
	served, failedOnce                         map[string]bool
}

type live struct {
	site    *site
	started time.Time
}

func (s *site) enter() *live {
	now := s.live.Add(1)
	for {
		peak := s.peak.Load()
		if now <= peak || s.peak.CompareAndSwap(peak, now) {
			break
		}
	}
	return &live{site: s, started: time.Now()}
}

func (l *live) finish() {
	l.site.finished.Add(1)
	l.site.mu.Lock()
	l.site.latencies = append(l.site.latencies, time.Since(l.started).Microseconds())
	l.site.mu.Unlock()
}

func (l *live) exit() { l.site.live.Add(-1) }

func (s *site) attempt(ctx context.Context, key string, item int) (uint64, error) {
	s.attempts.Add(1)
	pause := map[string]int{"throughput": 1, "storm": 1, "cancel": 50, "deadline": 2_000}[s.spec.scenario]
	if s.spec.scenario == "failfast" {
		pause = item*7_919%20 + 1
	}
	timer := time.NewTimer(time.Duration(pause) * time.Millisecond)
	defer timer.Stop()
	select {
	case <-timer.C:
	case <-ctx.Done():
		return 0, ctx.Err()
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	switch {
	case s.spec.scenario == "storm":
		return 0, fmt.Errorf("the upstream is down: %w", errTransient)
	case s.spec.scenario == "failfast" && item == 500:
		return 0, errors.New("malformed record")
	case s.spec.scenario == "throughput" && item%97 == 0 && !s.failedOnce[key]:
		s.failedOnce[key] = true
		return 0, fmt.Errorf("the site was busy: %w", errTransient)
	}
	if s.served[key] {
		s.duplicates.Add(1)
	}
	s.served[key] = true
	return uint64(item), nil
}

// BEGIN errgroup
func one(ctx context.Context, s *site, tenant string, item int) (uint64, error) {
	live := s.enter()
	defer live.exit()
	key := fmt.Sprintf("%s/%d", tenant, item)
	for attempt := 1; ; attempt++ {
		attemptCtx, cancel := context.WithTimeout(ctx, s.spec.deadline)
		value, err := s.attempt(attemptCtx, key, item)
		cancel()
		if err == nil {
			live.finish()
			return value, nil
		}
		if errors.Is(err, context.DeadlineExceeded) && ctx.Err() == nil {
			err = fmt.Errorf("timed out: %w", errTransient)
		}
		if !errors.Is(err, errTransient) || attempt >= s.spec.attempts {
			return 0, err
		}
		select {
		case <-time.After(s.spec.wait):
		case <-ctx.Done():
			return 0, ctx.Err()
		}
	}
}

func runItems(ctx context.Context, s *site, tenant string, items []int) (uint64, error) {
	group, ctx := errgroup.WithContext(ctx)
	group.SetLimit(s.spec.bound)
	var total atomic.Uint64
	for _, item := range items {
		if ctx.Err() != nil {
			break
		}
		group.Go(func() error {
			value, err := one(ctx, s, tenant, item)
			total.Add(value)
			return err
		})
	}
	if err := group.Wait(); err != nil {
		return 0, err
	}
	return total.Load(), nil
}

// END errgroup

func quantile(values []int64, permille int) float64 {
	if len(values) == 0 {
		return 0
	}
	return float64(values[(len(values)-1)*permille/1_000]) / 1_000
}

func main() {
	scenario := "throughput"
	if len(os.Args) > 1 {
		scenario = os.Args[1]
	}
	sp, err := specOf(scenario)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	s := &site{spec: sp, served: map[string]bool{}, failedOnce: map[string]bool{}}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	started := time.Now()
	tenants := []string{"acme", "globex"}[:sp.tenants]
	totals := make([]uint64, len(tenants))
	errs := make([]error, len(tenants))
	var wait sync.WaitGroup
	for index, tenant := range tenants {
		wait.Add(1)
		go func() {
			defer wait.Done()
			items := make([]int, sp.items)
			for item := range items {
				items[item] = item
			}
			totals[index], errs[index] = runItems(ctx, s, tenant, items)
		}()
	}
	if sp.cancelAfter > 0 {
		time.Sleep(sp.cancelAfter)
		cancel()
	}
	wait.Wait()
	elapsed := time.Since(started)
	liveAtReturn, finishedAtReturn := s.live.Load(), s.finished.Load()
	time.Sleep(200 * time.Millisecond)
	var total uint64
	message := ""
	for index := range tenants {
		total += totals[index]
		if errs[index] != nil && message == "" {
			message = errs[index].Error()
		}
	}
	items := sp.items * sp.tenants
	s.mu.Lock()
	latencies := append([]int64(nil), s.latencies...)
	s.mu.Unlock()
	sort.Slice(latencies, func(a, b int) bool { return latencies[a] < latencies[b] })
	perS := 0
	if message == "" {
		perS = int(float64(items) / elapsed.Seconds())
	} else {
		total = 0
	}
	line, _ := json.Marshal(struct {
		Way                 string  `json:"way"`
		Scenario            string  `json:"scenario"`
		Ok                  bool    `json:"ok"`
		Items               int     `json:"items"`
		Total               uint64  `json:"total"`
		WallMs              float64 `json:"wall_ms"`
		PerS                int     `json:"per_s"`
		P50                 float64 `json:"p50_ms"`
		P95                 float64 `json:"p95_ms"`
		P99                 float64 `json:"p99_ms"`
		Bound               int     `json:"bound"`
		Peak                int64   `json:"peak_in_flight"`
		Attempts            int64   `json:"attempts"`
		Duplicates          int64   `json:"duplicates"`
		LiveAtReturn        int64   `json:"live_at_return"`
		LiveAfterGrace      int64   `json:"live_after_grace"`
		FinishedAfterReturn int64   `json:"finished_after_return"`
		Error               string  `json:"error"`
	}{"go-errgroup", scenario, message == "", items, total, float64(elapsed.Microseconds()) / 1_000, perS,
		quantile(latencies, 500), quantile(latencies, 950), quantile(latencies, 990), sp.bound, s.peak.Load(),
		s.attempts.Load(), s.duplicates.Load(), liveAtReturn, s.live.Load(), s.finished.Load() - finishedAtReturn, message})
	fmt.Println(string(line))
}
