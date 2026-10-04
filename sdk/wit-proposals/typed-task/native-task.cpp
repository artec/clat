// Author-only typed AsyncTask bridge. No guest integers or JS polling.
#include "extension-api.h"
#include "builtins/web/abort/abort-signal.h"
#include "host-apis/wasi-0.2.10/bindings/bindings.h"
#include "js/Array.h"
#include "js/Object.h"
#include "js/Conversions.h"
#include "js/Promise.h"
#include "js/String.h"
#include "js/CharacterEncoding.h"
#include "js/experimental/TypedData.h"
#include <cstring>
#include <cmath>
#include <vector>

namespace clat::net_task {
using namespace JS;
using builtins::web::abort::AbortSignal;
static api::Engine *ENGINE;
static const JSClass ResolutionClass = {"ClatNetResolution", JSCLASS_HAS_RESERVED_SLOTS(1)};
static const JSClass ResponseClass = {"ClatNetResponse", JSCLASS_HAS_RESERVED_SLOTS(1)};
static const JSClass ChunkClass = {"ClatNetChunk", 0};

static bool failure(JSContext *cx, clat_net_task_egress_failure_t code) {
  static const char *names[] = {"invalid-request", "capability-denied", "permission-denied",
    "blocked-address", "name-resolution-failed", "invalid-resolution", "deadline-exceeded",
    "cancelled", "transport-failed", "limit-exceeded", "unsupported", "consumed"};
  JS_ReportErrorASCII(cx, "%s", code < 12 ? names[code] : "unsupported");
  return false;
}
static JSObject *wrap(JSContext *cx, int32_t handle, bool resolution) {
  RootedObject proto(cx, nullptr);
  RootedObject value(cx, JS_NewObjectWithGivenProto(cx, resolution ? &ResolutionClass : &ResponseClass, proto));
  if (value) SetReservedSlot(value, 0, Int32Value(handle));
  return value;
}
static bool handle(JSContext *cx, HandleValue value, bool resolution, int32_t *out) {
  if (!value.isObject() || GetClass(&value.toObject()) != (resolution ? &ResolutionClass : &ResponseClass)) {
    JS_ReportErrorASCII(cx, "opaque network resource required");
    return false;
  }
  Value slot = GetReservedSlot(&value.toObject(), 0);
  if (!slot.isInt32() || slot.toInt32() < 0) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_CONSUMED);
  *out = slot.toInt32();
  return true;
}
static void drop_outcome(clat_net_task_egress_outcome_t &out) {
  if (out.tag == 0) clat_net_task_egress_resolution_drop_own(out.val.resolved);
  else if (out.tag == 1) clat_net_task_egress_response_drop_own(out.val.headers);
  else if (out.tag == 2) clat_net_task_egress_response_drop_own(out.val.body.response);
}
static bool outcome(JSContext *cx, clat_net_task_egress_outcome_t &out, MutableHandleValue result) {
  int32_t rep = out.tag == 0 ? out.val.resolved.__handle : out.tag == 1
      ? out.val.headers.__handle : out.val.body.response.__handle;
  RootedObject resource(cx, wrap(cx, rep, out.tag == 0));
  if (!resource) return false;
  if (out.tag != 2) { result.setObject(*resource); return true; }
  if (out.val.body.bytes.len > 65536) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_LIMIT_EXCEEDED);
  RootedObject bytes(cx, JS_NewUint8Array(cx, out.val.body.bytes.len));
  RootedObject proto(cx, nullptr);
  RootedObject chunk(cx, JS_NewObjectWithGivenProto(cx, &ChunkClass, proto));
  if (!bytes || !chunk) return false;
  {
    AutoCheckCannotGC nogc;
    bool shared;
    auto *data = JS_GetUint8ArrayData(bytes, &shared, nogc);
    if (out.val.body.bytes.len) std::memcpy(data, out.val.body.bytes.ptr, out.val.body.bytes.len);
  }
  RootedValue resource_value(cx, ObjectValue(*resource)), bytes_value(cx, ObjectValue(*bytes));
  if (!JS_DefineProperty(cx, chunk, "response", resource_value, JSPROP_ENUMERATE) ||
      !JS_DefineProperty(cx, chunk, "bytes", bytes_value, JSPROP_ENUMERATE)) return false;
  result.setObject(*chunk);
  return true;
}
class NetworkTask final : public api::AsyncTask {
  clat_net_task_egress_own_task_t task_;
  Heap<JSObject *> promise_;
  bool closed_ = false;
  void close() {
    if (closed_) return;
    closed_ = true;
    if (handle_ != INVALID_POLLABLE_HANDLE) wasi_io_poll_pollable_drop_own({handle_});
    handle_ = INVALID_POLLABLE_HANDLE;
    clat_net_task_egress_task_drop_own(task_);
  }
public:
  NetworkTask(clat_net_task_egress_own_task_t task, HandleObject promise) : task_(task), promise_(promise) {}
  ~NetworkTask() override { close(); }
  bool subscribe(JSContext *cx) {
    clat_net_task_egress_own_pollable_t pollable{};
    clat_net_task_egress_failure_t error{};
    if (!clat_net_task_egress_method_task_subscribe({task_.__handle}, &pollable, &error)) return failure(cx, error);
    handle_ = pollable.__handle;
    return true;
  }
  bool run(api::Engine *engine) override {
    clat_net_task_egress_result_outcome_failure_t result{};
    if (!clat_net_task_egress_method_task_get({task_.__handle}, &result)) {
      engine->queue_async_task(this);
      return true;
    }
    auto *cx = engine->cx();
    RootedObject promise(cx, promise_);
    RootedValue value(cx);
    bool converted = !result.is_err && outcome(cx, result.val.ok, &value);
    if (result.is_err) failure(cx, result.val.err);
    if (!converted && !result.is_err) drop_outcome(result.val.ok);
    bool ok = converted ? ResolvePromise(cx, promise, value) : RejectPromiseWithPendingError(cx, promise);
    if (converted && !ok) drop_outcome(result.val.ok);
    // Generated free releases lists only; owned credentials were transferred above.
    clat_net_task_egress_result_outcome_failure_free(&result);
    close();
    return ok;
  }
  bool cancel(api::Engine *) override {
    if (!closed_) clat_net_task_egress_method_task_cancel({task_.__handle});
    close();
    return true;
  }
  bool abort(JSContext *cx, HandleObject signal) {
    RootedObject promise(cx, promise_);
    RootedValue reason(cx, AbortSignal::reason(signal));
    if (!RejectPromise(cx, promise, reason)) return false;
    return ENGINE->cancel_async_task(this);
  }
  void trace(JSTracer *trc) override { TraceEdge(trc, &promise_, "typed network promise"); }
};
struct TaskAborter final : builtins::web::abort::AbortAlgorithm {
  mozilla::WeakPtr<NetworkTask> task;
  Heap<JSObject *> signal;
  TaskAborter(NetworkTask *task, HandleObject signal) : task(task), signal(signal) {}
  bool run(JSContext *cx) override {
    RootedObject rooted(cx, signal);
    return !task || task->abort(cx, rooted);
  }
  void trace(JSTracer *trc) override { TraceEdge(trc, &signal, "typed network signal"); }
};
static bool queue(JSContext *cx, clat_net_task_egress_own_task_t task, HandleObject promise, HandleObject signal) {
  auto pending = RefPtr<NetworkTask>(js_new<NetworkTask>(task, promise));
  if (!pending) { clat_net_task_egress_task_drop_own(task); return false; }
  if (!pending->subscribe(cx)) return RejectPromiseWithPendingError(cx, promise);
  if (!AbortSignal::add_algorithm(signal, js::MakeUnique<TaskAborter>(pending.get(), signal))) return false;
  ENGINE->queue_async_task(pending);
  return true;
}
static bool promise(JSContext *cx, HandleValue signal_value, MutableHandleObject signal, MutableHandleObject result) {
  if (!AbortSignal::is_instance(signal_value)) { JS_ReportErrorASCII(cx, "AbortSignal required"); return false; }
  signal.set(&signal_value.toObject());
  result.set(NewPromiseObject(cx, nullptr));
  return !!result;
}
static bool uint_arg(JSContext *cx, HandleValue value, uint32_t limit, uint32_t *out) {
  if (!value.isNumber() || !std::isfinite(value.toNumber()) || value.toNumber() < 1 || value.toNumber() > limit ||
      value.toNumber() != uint32_t(value.toNumber())) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
  *out = uint32_t(value.toNumber());
  return true;
}
static bool utf8_arg(JSContext *cx, HandleValue value, size_t limit,
                     JS::UniqueChars &text, bindings_string_t &wit) {
  if (!value.isString() || JS_GetStringLength(value.toString()) > limit)
    return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
  RootedString string(cx, value.toString());
  auto *linear = JS_EnsureLinearString(cx, string);
  if (!linear) return false;
  size_t len = GetDeflatedUTF8StringLength(linear);
  if (len > limit) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_LIMIT_EXCEEDED);
  text = JS_EncodeStringToUTF8(cx, string);
  if (!text) return false;
  if (std::memchr(text.get(), 0, len)) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
  wit = {reinterpret_cast<uint8_t *>(text.get()), len};
  return true;
}
static bool text_arg(JSContext *cx, HandleValue value, JS::UniqueChars &text, bindings_string_t &wit) {
  if (!utf8_arg(cx, value, 2048, text, wit)) return false;
  if (wit.len != JS_GetStringLength(value.toString()))
    return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
  return true;
}
struct RequestStorage {
  JS::UniqueChars url, verb;
  std::vector<JS::UniqueChars> text;
  std::vector<clat_net_task_egress_header_t> headers;
  std::vector<uint8_t> body;
};
static bool array_arg(JSContext *cx, HandleValue value, uint32_t max,
                      MutableHandleObject object, uint32_t *length) {
  bool array = false;
  if (!IsArrayObject(cx, value, &array)) return false;
  if (!array) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
  object.set(&value.toObject());
  if (!GetArrayLength(cx, object, length)) return false;
  if (*length > max) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_LIMIT_EXCEEDED);
  return true;
}
static bool headers_arg(JSContext *cx, HandleValue value, RequestStorage &storage) {
  RootedObject array(cx), pair(cx);
  uint32_t count, pair_length;
  if (!array_arg(cx, value, 64, &array, &count)) return false;
  size_t total = 0;
  for (uint32_t i = 0; i < count; i++) {
    RootedValue item(cx), name(cx), content(cx);
    if (!JS_GetElement(cx, array, i, &item) || !array_arg(cx, item, 2, &pair, &pair_length)) return false;
    if (pair_length != 2) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
    if (!JS_GetElement(cx, pair, 0, &name) || !JS_GetElement(cx, pair, 1, &content)) return false;
    JS::UniqueChars name_text, value_text;
    clat_net_task_egress_header_t header{};
    if (!utf8_arg(cx, name, 32768, name_text, header.name) ||
        !utf8_arg(cx, content, 32768, value_text, header.value)) return false;
    total += header.name.len + header.value.len;
    if (total > 32768) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_LIMIT_EXCEEDED);
    storage.text.push_back(std::move(name_text));
    storage.text.push_back(std::move(value_text));
    storage.headers.push_back(header);
  }
  return true;
}
static bool body_arg(JSContext *cx, HandleValue value, RequestStorage &storage) {
  if (!value.isObject() || !JS_IsUint8Array(&value.toObject()))
    return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
  RootedObject bytes(cx, &value.toObject());
  size_t len = JS_GetArrayBufferViewByteLength(bytes);
  if (len > 1048576) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_LIMIT_EXCEEDED);
  storage.body.resize(len);
  bool shared;
  {
    AutoCheckCannotGC nogc;
    auto *data = JS_GetUint8ArrayData(bytes, &shared, nogc);
    if (!shared && len) std::memcpy(storage.body.data(), data, len);
  }
  if (shared) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
  return true;
}
static bool method_arg(JSContext *cx, HandleValue value, RequestStorage &storage,
                       clat_net_task_egress_method_t *verb) {
  bindings_string_t text{};
  if (!utf8_arg(cx, value, 7, storage.verb, text)) return false;
  static const char *names[] = {"GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"};
  for (uint8_t i = 0; i < 7; i++) {
    if (!std::strcmp(storage.verb.get(), names[i])) { *verb = i; return true; }
  }
  return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
}
static bool preabort(JSContext *cx, HandleObject signal, HandleObject promise) {
  RootedValue reason(cx, AbortSignal::reason(signal));
  return RejectPromise(cx, promise, reason);
}
bool dns(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  if (!args.requireAtLeast(cx, "clatNetDns", 3)) return false;
  JS::UniqueChars text;
  bindings_string_t origin{};
  uint32_t timeout;
  if (!text_arg(cx, args[0], text, origin) || !uint_arg(cx, args[1], 30000, &timeout)) return false;
  RootedObject signal(cx), result(cx);
  if (!promise(cx, args[2], &signal, &result)) return false;
  args.rval().setObject(*result);
  if (AbortSignal::is_aborted(signal)) return preabort(cx, signal, result);
  clat_net_task_egress_own_task_t task{};
  clat_net_task_egress_failure_t error{};
  if (!clat_net_task_egress_dns_start(&origin, timeout, &task, &error)) {
    failure(cx, error); return RejectPromiseWithPendingError(cx, result);
  }
  return queue(cx, task, result, signal);
}
bool http(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  if (!args.requireAtLeast(cx, "clatNetHttp", 5)) return false;
  int32_t rep;
  JS::UniqueChars text;
  clat_net_task_egress_request_t request{};
  if (!handle(cx, args[0], true, &rep) || !text_arg(cx, args[1], text, request.url) ||
      !uint_arg(cx, args[2], 30000, &request.timeout_ms) || !uint_arg(cx, args[3], 8388608, &request.max_response_bytes)) return false;
  request.verb = CLAT_NET_TASK_EGRESS_METHOD_GET;
  RootedObject signal(cx), result(cx);
  if (!promise(cx, args[4], &signal, &result)) return false;
  args.rval().setObject(*result);
  if (AbortSignal::is_aborted(signal)) return preabort(cx, signal, result);
  SetReservedSlot(&args[0].toObject(), 0, Int32Value(-1));
  clat_net_task_egress_own_task_t task{};
  clat_net_task_egress_failure_t error{};
  if (!clat_net_task_egress_http_start({rep}, &request, &task, &error)) {
    failure(cx, error); return RejectPromiseWithPendingError(cx, result);
  }
  return queue(cx, task, result, signal);
}
// Conversion can run guest getters. Validate ownership at the final transfer boundary.
bool request(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  if (!args.requireAtLeast(cx, "clatNetRequest", 8)) return false;
  int32_t rep;
  if (!handle(cx, args[0], true, &rep)) return false;
  RequestStorage storage;
  clat_net_task_egress_request_t request{};
  if (!text_arg(cx, args[1], storage.url, request.url) ||
      !method_arg(cx, args[2], storage, &request.verb) ||
      !uint_arg(cx, args[5], 30000, &request.timeout_ms) ||
      !uint_arg(cx, args[6], 8388608, &request.max_response_bytes) ||
      !body_arg(cx, args[4], storage) || !headers_arg(cx, args[3], storage)) return false;
  RootedObject signal(cx), result(cx);
  if (!promise(cx, args[7], &signal, &result)) return false;
  args.rval().setObject(*result);
  if (!handle(cx, args[0], true, &rep)) return RejectPromiseWithPendingError(cx, result);
  if (AbortSignal::is_aborted(signal)) return preabort(cx, signal, result);
  request.headers = {storage.headers.data(), storage.headers.size()};
  request.body = {storage.body.data(), storage.body.size()};
  SetReservedSlot(&args[0].toObject(), 0, Int32Value(-1));
  clat_net_task_egress_own_task_t task{};
  clat_net_task_egress_failure_t error{};
  if (!clat_net_task_egress_http_start({rep}, &request, &task, &error)) {
    failure(cx, error); return RejectPromiseWithPendingError(cx, result);
  }
  return queue(cx, task, result, signal);
}
bool read(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  if (!args.requireAtLeast(cx, "clatNetRead", 3)) return false;
  int32_t rep; uint32_t max;
  if (!handle(cx, args[0], false, &rep) || !uint_arg(cx, args[1], 65536, &max)) return false;
  RootedObject signal(cx), result(cx);
  if (!promise(cx, args[2], &signal, &result)) return false;
  args.rval().setObject(*result);
  if (AbortSignal::is_aborted(signal)) return preabort(cx, signal, result);
  SetReservedSlot(&args[0].toObject(), 0, Int32Value(-1));
  clat_net_task_egress_own_task_t task{};
  clat_net_task_egress_failure_t error{};
  if (!clat_net_task_egress_read_start({rep}, max, &task, &error)) {
    failure(cx, error); return RejectPromiseWithPendingError(cx, result);
  }
  return queue(cx, task, result, signal);
}
bool dispose(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  if (!args.requireAtLeast(cx, "clatNetDispose", 1)) return false;
  bool resolution = args[0].isObject() && GetClass(&args[0].toObject()) == &ResolutionClass;
  if (!args[0].isObject() || (!resolution && GetClass(&args[0].toObject()) != &ResponseClass)) return failure(cx, CLAT_NET_TASK_EGRESS_FAILURE_INVALID_REQUEST);
  auto rep = GetReservedSlot(&args[0].toObject(), 0).toInt32();
  if (rep >= 0) {
    SetReservedSlot(&args[0].toObject(), 0, Int32Value(-1));
    if (resolution) clat_net_task_egress_resolution_drop_own({rep});
    else clat_net_task_egress_response_drop_own({rep});
  }
  args.rval().setUndefined();
  return true;
}
static bool address_value(JSContext *cx, const clat_net_task_egress_address_t &address,
                          bool detailed, MutableHandleValue result) {
  RootedString ip(cx, JS_NewStringCopyUTF8N(cx, UTF8Chars(reinterpret_cast<char *>(address.ip.ptr), address.ip.len)));
  if (!ip) return false;
  result.setString(ip);
  if (!detailed) return true;
  RootedObject pair(cx, NewArrayObject(cx, 2));
  RootedString family(cx, JS_NewStringCopyZ(cx, address.family == 0 ? "ipv4" : "ipv6"));
  if (!pair || !family) return false;
  RootedValue family_value(cx, StringValue(family));
  if (!JS_DefineElement(cx, pair, 0, result, JSPROP_ENUMERATE) ||
      !JS_DefineElement(cx, pair, 1, family_value, JSPROP_ENUMERATE)) return false;
  result.setObject(*pair);
  return true;
}
static bool metadata(JSContext *cx, unsigned argc, Value *vp, bool dns64, bool detailed = false) {
  auto args = CallArgsFromVp(argc, vp);
  if (!args.requireAtLeast(cx, "clatNetInfo", 1)) return false;
  bool resolution = args[0].isObject() && GetClass(&args[0].toObject()) == &ResolutionClass;
  int32_t rep;
  if (!handle(cx, args[0], resolution, &rep)) return false;
  clat_net_task_egress_failure_t error{};
  if (!resolution) {
    uint16_t status;
    if (!clat_net_task_egress_method_response_status({rep}, &status, &error)) return failure(cx, error);
    args.rval().setInt32(status); return true;
  }
  clat_net_task_egress_list_address_t addresses{};
  bool fetched = dns64 ? clat_net_task_egress_method_resolution_dns64_answers({rep}, &addresses, &error)
    : clat_net_task_egress_method_resolution_addresses({rep}, &addresses, &error);
  if (!fetched) return failure(cx, error);
  RootedObject array(cx, NewArrayObject(cx, addresses.len));
  bool ok = !!array;
  for (size_t i = 0; ok && i < addresses.len; i++) {
    RootedValue value(cx);
    ok = address_value(cx, addresses.ptr[i], detailed, &value) &&
         JS_DefineElement(cx, array, i, value, JSPROP_ENUMERATE);
  }
  clat_net_task_egress_list_address_free(&addresses);
  if (ok) args.rval().setObject(*array);
  return ok;
}
bool info(JSContext *cx, unsigned argc, Value *vp) { return metadata(cx, argc, vp, false); }
bool address_info(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  int32_t rep;
  if (!args.requireAtLeast(cx, "clatNetAddresses", 1) || !handle(cx, args[0], true, &rep)) return false;
  return metadata(cx, argc, vp, false, true);
}
bool dns64(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  int32_t rep;
  if (!args.requireAtLeast(cx, "clatNetDns64", 1) || !handle(cx, args[0], true, &rep)) return false;
  return metadata(cx, argc, vp, true, true);
}
bool response_headers(JSContext *cx, unsigned argc, Value *vp) {
  auto args = CallArgsFromVp(argc, vp);
  int32_t rep;
  if (!args.requireAtLeast(cx, "clatNetHeaders", 1) || !handle(cx, args[0], false, &rep)) return false;
  clat_net_task_egress_failure_t error{};
  clat_net_task_egress_list_header_t headers{};
  if (!clat_net_task_egress_method_response_headers({rep}, &headers, &error)) return failure(cx, error);
  RootedObject array(cx, NewArrayObject(cx, headers.len));
  bool ok = !!array;
  for (size_t i = 0; ok && i < headers.len; i++) {
    RootedObject pair(cx, NewArrayObject(cx, 2));
    RootedString name(cx, JS_NewStringCopyUTF8N(cx, UTF8Chars(reinterpret_cast<char *>(headers.ptr[i].name.ptr), headers.ptr[i].name.len)));
    RootedString content(cx, JS_NewStringCopyUTF8N(cx, UTF8Chars(reinterpret_cast<char *>(headers.ptr[i].value.ptr), headers.ptr[i].value.len)));
    RootedValue name_value(cx), content_value(cx), pair_value(cx);
    ok = pair && name && content;
    if (ok) {
      name_value.setString(name); content_value.setString(content); pair_value.setObject(*pair);
      ok = JS_DefineElement(cx, pair, 0, name_value, JSPROP_ENUMERATE) &&
           JS_DefineElement(cx, pair, 1, content_value, JSPROP_ENUMERATE) &&
           JS_DefineElement(cx, array, i, pair_value, JSPROP_ENUMERATE);
    }
  }
  clat_net_task_egress_list_header_free(&headers);
  if (ok) args.rval().setObject(*array);
  return ok;
}
bool install(api::Engine *engine) {
  ENGINE = engine;
  auto *cx = engine->cx();
  return JS_DefineFunction(cx, engine->global(), "clatNetDns", dns, 3, 0) &&
    JS_DefineFunction(cx, engine->global(), "clatNetHttp", http, 5, 0) &&
    JS_DefineFunction(cx, engine->global(), "clatNetRead", read, 3, 0) &&
    JS_DefineFunction(cx, engine->global(), "clatNetRequest", request, 8, 0) &&
    JS_DefineFunction(cx, engine->global(), "clatNetDns64", dns64, 1, 0) &&
    JS_DefineFunction(cx, engine->global(), "clatNetAddresses", address_info, 1, 0) &&
    JS_DefineFunction(cx, engine->global(), "clatNetHeaders", response_headers, 1, 0) &&
    JS_DefineFunction(cx, engine->global(), "clatNetInfo", info, 1, 0) &&
    JS_DefineFunction(cx, engine->global(), "clatNetDispose", dispose, 1, 0);
}
} // namespace clat::net_task
