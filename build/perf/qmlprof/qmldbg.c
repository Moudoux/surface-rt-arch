/* LD_PRELOAD helper: turn on QML debugging in apps not built with QT_QML_DEBUG,
 * so -qmljsdebugger=port:N works (qmlprofiler). QQmlDebuggingEnabler::enableDebugging(bool) */
extern void _ZN20QQmlDebuggingEnabler15enableDebuggingEb(int printWarning);
__attribute__((constructor)) static void qmldbg_init(void) { _ZN20QQmlDebuggingEnabler15enableDebuggingEb(1); }
