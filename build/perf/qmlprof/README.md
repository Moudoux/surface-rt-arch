# Launcher (Kickoff) CPU profiling tools

Run on the Surface from /tmp/perf or /tmp/pf (copy them there). plasmashell as transient unit `ps-perf`
(restore: `systemctl --user stop ps-perf; systemctl --user start plasma-plasmashell`).

- `lcost.sh [cycles] [unit]`: plasmashell CPU ms per launcher open / close (3 s each).
- `lthr.sh [unit]`: same, split per thread (GUI vs QSGRenderThread).
- `psab.sh label [systemd-run -E ...]`: restart ps-perf with an env, then lcost.sh 4 (A/B helper).
- `lprof.sh [unit] [perf args]` (F=Hz): perf record of one open / close. perf runs from tmpfs:
  perf + libtraceevent + libpfm from the ALARM mirror unpacked into /tmp/pf/root (no install).
  Only flat profiles work: neither --call-graph dwarf nor fp unwinds on this armv7 userland.
- `qmldbg.c` -> `qmldbg.so` (gcc -O2 -shared -fPIC, built in srt-kde-build): LD_PRELOAD that calls
  QQmlDebuggingEnabler::enableDebugging so a release plasmashell accepts
  `-qmljsdebugger=port:3768,host:127.0.0.1,services:CanvasFrameRate,EngineControl,DebugMessages`.
- `qprof.sh out.qtd`: qmlprofiler (/usr/lib/qt6/bin) trace of one launcher toggle.
- `qtdsum.py *.qtd` (host): self / inclusive time per QML binding / JS / signal location.
- `traces/`: the open/close traces from the measurement (Plasma 6.8.2, Qt 6.12.0).
