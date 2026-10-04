// Isolated semantic task scheduling probe, not the production net ABI.
#include "extension-api.h"
#include "builtins/web/abort/abort-signal.h"
#include "host-apis/wasi-0.2.10/bindings/bindings.h"
#include "js/Conversions.h"
#include "js/Promise.h"
#include "js/String.h"
#include <string_view>

namespace clat::net_task_probe {
using namespace JS;
using builtins::web::abort::AbortSignal;
static api::Engine *ENGINE;

class NetworkTask final : public api::AsyncTask {
  clat_net_task_probe_tasks_own_job_t job_;
  Heap<JSObject *> promise_;
  bool closed_ = false;

  void close() {
    if (closed_) return;
    closed_ = true;
    wasi_io_poll_pollable_drop_own({handle_});
    handle_ = INVALID_POLLABLE_HANDLE;
    clat_net_task_probe_tasks_job_drop_own(job_);
  }

public:
  NetworkTask(clat_net_task_probe_tasks_own_job_t job, HandleObject promise)
      : job_(job), promise_(promise) {
    handle_ = clat_net_task_probe_tasks_method_job_subscribe({job_.__handle}).__handle;
  }
  ~NetworkTask() override { close(); }

  bool run(api::Engine *engine) override {
    clat_net_task_probe_tasks_result_string_failure_t result{};
    if (!clat_net_task_probe_tasks_method_job_get({job_.__handle}, &result)) {
      engine->queue_async_task(this);
      return true;
    }
    auto *cx = engine->cx();
    RootedObject promise(cx, promise_);
    bool ok = false;
    if (result.is_err || result.val.ok.len > 65536) {
      JS_ReportErrorASCII(cx, "bounded network task failed");
      ok = RejectPromiseWithPendingError(cx, promise);
    } else {
      RootedString text(cx, JS_NewStringCopyUTF8N(cx,
          UTF8Chars(reinterpret_cast<char *>(result.val.ok.ptr), result.val.ok.len)));
      if (text) {
        RootedValue value(cx, StringValue(text));
        ok = ResolvePromise(cx, promise, value);
      }
    }
    clat_net_task_probe_tasks_result_string_failure_free(&result);
    close();
    return ok;
  }

  bool cancel(api::Engine *) override {
    if (!closed_) clat_net_task_probe_tasks_method_job_cancel({job_.__handle});
    close();
    return true;
  }

  bool abort(JSContext *cx, HandleObject signal) {
    RootedObject promise(cx, promise_);
    RootedValue reason(cx, AbortSignal::reason(signal));
    if (!RejectPromise(cx, promise, reason)) return false;
    return ENGINE->cancel_async_task(this);
  }
  void trace(JSTracer *trc) override { TraceEdge(trc, &promise_, "network task promise"); }
};

struct TaskAborter final : builtins::web::abort::AbortAlgorithm {
  mozilla::WeakPtr<NetworkTask> task;
  Heap<JSObject *> signal;
  TaskAborter(NetworkTask *task, HandleObject signal) : task(task), signal(signal) {}
  bool run(JSContext *cx) override {
    RootedObject rooted(cx, signal);
    return !task || task->abort(cx, rooted);
  }
  void trace(JSTracer *trc) override { TraceEdge(trc, &signal, "network task signal"); }
};

bool start(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  if (!args.requireAtLeast(cx, "clatNetTaskProbe", 2)) return false;
  if (!args[0].isString() || !AbortSignal::is_instance(args[1])) {
    JS_ReportErrorASCII(cx, "network task phase and AbortSignal required");
    return false;
  }
  RootedString phase_string(cx, args[0].toString());
  auto phase_utf8 = JS_EncodeStringToUTF8(cx, phase_string);
  if (!phase_utf8) return false;
  std::string_view phase(phase_utf8.get());
  if (JS_GetStringLength(phase_string) != phase.size() ||
      (phase != "dns" && phase != "headers" && phase != "body")) {
    JS_ReportErrorASCII(cx, "unsupported network task phase");
    return false;
  }
  RootedObject signal(cx, &args[1].toObject());
  RootedObject promise(cx, NewPromiseObject(cx, nullptr));
  if (!promise) return false;
  args.rval().setObject(*promise);
  if (AbortSignal::is_aborted(signal)) {
    RootedValue reason(cx, AbortSignal::reason(signal));
    return RejectPromise(cx, promise, reason);
  }
  auto kind = phase == "dns" ? CLAT_NET_TASK_PROBE_TASKS_PHASE_DNS
      : phase == "headers" ? CLAT_NET_TASK_PROBE_TASKS_PHASE_HEADERS
      : CLAT_NET_TASK_PROBE_TASKS_PHASE_BODY;
  clat_net_task_probe_tasks_own_job_t job{};
  clat_net_task_probe_tasks_failure_t failure{};
  if (!clat_net_task_probe_tasks_start(kind, &job, &failure)) {
    JS_ReportErrorASCII(cx, "bounded network task submission failed");
    return RejectPromiseWithPendingError(cx, promise);
  }
  auto task = RefPtr<NetworkTask>(js_new<NetworkTask>(job, promise));
  if (!AbortSignal::add_algorithm(signal, js::MakeUnique<TaskAborter>(task.get(), signal))) return false;
  ENGINE->queue_async_task(task);
  return true;
}

bool install(api::Engine *engine) {
  ENGINE = engine;
  return JS_DefineFunction(engine->cx(), engine->global(), "clatNetTaskProbe", start, 2, 0);
}
} // namespace clat::net_task_probe
