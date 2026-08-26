#include <orna_runtime_abi_v1.h>

#include <chrono>
#include <cstddef>
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cstring>

namespace {

constexpr OrnaNodeHandle kWindowNode = 100;
constexpr OrnaNodeHandle kPanelNode = 101;
constexpr OrnaNodeHandle kTextNode = 102;
constexpr OrnaNodeHandle kButtonNode = 103;
constexpr OrnaActionHandle kButtonAction = 200;

constexpr char kWindowTitle[] = "OrnaDB Qt runtime demo";
constexpr char kText[] = "ABI-backed Qt runtime";
constexpr char kButtonLabel[] = "Continue";
constexpr char kTextType[] = "std.text";
constexpr char kBooleanType[] = "std.boolean";
constexpr unsigned char kEnabled[] = {1};

constexpr char kInvalidMessage[] = "invalid demo callback input";
constexpr char kUnsupportedMessage[] = "demo callback is unsupported";

struct DemoState {
    std::size_t action_events = 0;
    std::size_t surface_closed_events = 0;
};

DemoState g_demo_state{};

OrnaStringView view(const char *value) noexcept {
    return OrnaStringView{value, value == nullptr ? 0 : std::strlen(value)};
}

OrnaBytesView bytes(const char *value) noexcept {
    return OrnaBytesView{
        reinterpret_cast<const std::uint8_t *>(value),
        value == nullptr ? 0 : std::strlen(value),
    };
}

OrnaStatus status(OrnaStatusCode code, const char *message) noexcept {
    return OrnaStatus{code, view(message)};
}

OrnaStatus ok_status() noexcept {
    return status(ORNA_STATUS_OK, "");
}

OrnaStatus invalid_status() noexcept {
    return status(ORNA_STATUS_INVALID_ARGUMENT, kInvalidMessage);
}

OrnaStatus unsupported_status() noexcept {
    return status(ORNA_STATUS_UNSUPPORTED, kUnsupportedMessage);
}

void clear_owned_bytes(OrnaOwnedBytes *output) noexcept {
    if (output != nullptr) {
        *output = OrnaOwnedBytes{nullptr, 0, nullptr, nullptr};
    }
}

void client_log(void *,
                std::uint32_t,
                OrnaStringView,
                OrnaStringView) noexcept {
    try {
        // The demo emits only its own concise diagnostics.
    } catch (...) {
    }
}

OrnaStatus client_emit_runtime_event(void *context,
                                     OrnaRuntimeHandle,
                                     const OrnaRuntimeEventV1 *event) noexcept {
    try {
        if (context == nullptr || event == nullptr) {
            return invalid_status();
        }
        auto *state = static_cast<DemoState *>(context);
        if (event->kind == ORNA_RUNTIME_EVENT_ACTION) {
            ++state->action_events;
        } else if (event->kind == ORNA_RUNTIME_EVENT_SURFACE_CLOSED) {
            ++state->surface_closed_events;
        }
        return ok_status();
    } catch (...) {
        return invalid_status();
    }
}

OrnaStatus client_complete_model_request(void *,
                                         OrnaRequestHandle,
                                         OrnaValueRefV1) noexcept {
    try {
        return unsupported_status();
    } catch (...) {
        return invalid_status();
    }
}

OrnaStatus client_fail_model_request(void *,
                                     OrnaRequestHandle,
                                     OrnaStatus failure) noexcept {
    try {
        return failure;
    } catch (...) {
        return invalid_status();
    }
}

OrnaStatus client_read_action_metadata(void *,
                                       OrnaActionHandle,
                                       OrnaOwnedBytes *output) noexcept {
    try {
        if (output == nullptr) {
            return invalid_status();
        }
        clear_owned_bytes(output);
        return unsupported_status();
    } catch (...) {
        return invalid_status();
    }
}

OrnaStatus client_read_value_debug_json(void *,
                                        OrnaValueRefV1,
                                        OrnaOwnedBytes *output) noexcept {
    try {
        if (output == nullptr) {
            return invalid_status();
        }
        clear_owned_bytes(output);
        return unsupported_status();
    } catch (...) {
        return invalid_status();
    }
}

std::uint64_t client_monotonic_time_ns(void *) noexcept {
    try {
        using Clock = std::chrono::steady_clock;
        const auto count =
            std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now().time_since_epoch()).count();
        return count > 0 ? static_cast<std::uint64_t>(count) : 0;
    } catch (...) {
        return 0;
    }
}

void report_status(const char *operation, OrnaStatus value) noexcept {
    const auto message = value.message;
    const auto message_length = message.len > 256 ? 256 : static_cast<int>(message.len);
    const char *message_data = message.data == nullptr ? "" : message.data;
    std::fprintf(stderr,
                 "orna-runtime-qt-demo: %s failed (%u): %.*s\n",
                 operation,
                 static_cast<unsigned>(value.code),
                 message_length,
                 message_data);
}

bool status_ok(const char *operation, OrnaStatus value) noexcept {
    if (value.code == ORNA_STATUS_OK) {
        return true;
    }
    report_status(operation, value);
    return false;
}

void release_owned_bytes(OrnaOwnedBytes *bytes_value) noexcept {
    if (bytes_value == nullptr || bytes_value->release == nullptr) {
        return;
    }
    try {
        bytes_value->release(bytes_value->owner, bytes_value->data, bytes_value->len);
    } catch (...) {
    }
    *bytes_value = OrnaOwnedBytes{nullptr, 0, nullptr, nullptr};
}

bool shutdown_runtime(const OrnaRuntimeApiV1 *api, OrnaRuntimeHandle runtime) noexcept {
    if (api == nullptr || runtime == 0) {
        return false;
    }
    const auto shutdown_status_value = api->request_shutdown(runtime);
    if (!status_ok("request_shutdown", shutdown_status_value)) {
        return false;
    }
    api->destroy(runtime);
    return true;
}

int run_demo(bool smoke) {
    g_demo_state = DemoState{};

    const auto *api = orna_runtime_query_v1();
    if (api == nullptr || api->create == nullptr || api->destroy == nullptr
        || api->request_shutdown == nullptr || api->create_surface == nullptr
        || api->apply_ui_batch == nullptr || api->capture_semantic_state == nullptr
        || api->poll_event_loop == nullptr) {
        std::fprintf(stderr, "orna-runtime-qt-demo: runtime API is incomplete\n");
        return EXIT_FAILURE;
    }

    const OrnaClientApiV1 client{
        ORNA_RUNTIME_ABI_V1_MAJOR,
        ORNA_RUNTIME_ABI_V1_MINOR,
        &g_demo_state,
        client_log,
        client_emit_runtime_event,
        client_complete_model_request,
        client_fail_model_request,
        client_read_action_metadata,
        client_read_value_debug_json,
        client_monotonic_time_ns,
    };
    const OrnaRuntimeCreateOptionsV1 runtime_options{
        &client,
        view("en-GB"),
        view("UTC"),
        view("light"),
        OrnaStringView{nullptr, 0},
        OrnaStringView{nullptr, 0},
    };

    OrnaRuntimeHandle runtime = 0;
    if (!status_ok("create", api->create(&runtime_options, &runtime))) {
        return EXIT_FAILURE;
    }

    const OrnaSurfaceCreateOptionsV1 surface_options{
        view("window"),
        view(kWindowTitle),
        view("local"),
        OrnaBytesView{nullptr, 0},
    };
    OrnaSurfaceHandle surface = 0;
    if (!status_ok("create_surface", api->create_surface(runtime, &surface_options, &surface))) {
        (void)shutdown_runtime(api, runtime);
        return EXIT_FAILURE;
    }

    const OrnaValueRefV1 no_key{
        0,
        OrnaStringView{nullptr, 0},
        OrnaBytesView{nullptr, 0},
    };
    const OrnaMountNodeV1 window_mount{
        kWindowNode,
        0,
        view("root"),
        0,
        view("std.ui.window"),
        1,
        0,
        no_key,
    };
    const OrnaMountNodeV1 panel_mount{
        kPanelNode,
        kWindowNode,
        view("content"),
        0,
        view("std.ui.panel"),
        1,
        0,
        no_key,
    };
    const OrnaMountNodeV1 text_mount{
        kTextNode,
        kPanelNode,
        view("content"),
        0,
        view("std.ui.text"),
        1,
        0,
        no_key,
    };
    const OrnaMountNodeV1 button_mount{
        kButtonNode,
        kPanelNode,
        view("content"),
        1,
        view("std.ui.button"),
        1,
        0,
        no_key,
    };

    const OrnaSetPropertyV1 window_title_property{
        kWindowNode,
        view("title"),
        OrnaValueRefV1{0, view(kTextType), bytes(kWindowTitle)},
    };
    const OrnaSetPropertyV1 text_property{
        kTextNode,
        view("text"),
        OrnaValueRefV1{0, view(kTextType), bytes(kText)},
    };
    const OrnaSetPropertyV1 button_label_property{
        kButtonNode,
        view("label"),
        OrnaValueRefV1{0, view(kTextType), bytes(kButtonLabel)},
    };
    const OrnaSetPropertyV1 button_enabled_property{
        kButtonNode,
        view("enabled"),
        OrnaValueRefV1{
            0,
            view(kBooleanType),
            OrnaBytesView{reinterpret_cast<const std::uint8_t *>(kEnabled), sizeof(kEnabled)},
        },
    };
    const OrnaBindActionV1 button_action{
        kButtonNode,
        view("clicked"),
        kButtonAction,
        view(kTextType),
    };

    OrnaUiOperationV1 operations[9]{};
    operations[0].kind = ORNA_UI_OP_MOUNT_NODE;
    operations[0].as.mount_node = window_mount;
    operations[1].kind = ORNA_UI_OP_MOUNT_NODE;
    operations[1].as.mount_node = panel_mount;
    operations[2].kind = ORNA_UI_OP_MOUNT_NODE;
    operations[2].as.mount_node = text_mount;
    operations[3].kind = ORNA_UI_OP_MOUNT_NODE;
    operations[3].as.mount_node = button_mount;
    operations[4].kind = ORNA_UI_OP_SET_PROPERTY;
    operations[4].as.set_property = window_title_property;
    operations[5].kind = ORNA_UI_OP_SET_PROPERTY;
    operations[5].as.set_property = text_property;
    operations[6].kind = ORNA_UI_OP_SET_PROPERTY;
    operations[6].as.set_property = button_label_property;
    operations[7].kind = ORNA_UI_OP_SET_PROPERTY;
    operations[7].as.set_property = button_enabled_property;
    operations[8].kind = ORNA_UI_OP_BIND_ACTION;
    operations[8].as.bind_action = button_action;

    const OrnaUiBatchV1 batch{
        1,
        operations,
        sizeof(operations) / sizeof(operations[0]),
    };
    if (!status_ok("apply_ui_batch", api->apply_ui_batch(runtime, surface, &batch))) {
        (void)shutdown_runtime(api, runtime);
        return EXIT_FAILURE;
    }

    OrnaOwnedBytes canonical_state{nullptr, 0, nullptr, nullptr};
    const auto capture_status = api->capture_semantic_state(runtime, surface, &canonical_state);
    if (!status_ok("capture_semantic_state", capture_status)) {
        release_owned_bytes(&canonical_state);
        (void)shutdown_runtime(api, runtime);
        return EXIT_FAILURE;
    }
    std::printf("orna-runtime-qt-demo: canonical state %zu bytes\n", canonical_state.len);
    release_owned_bytes(&canonical_state);

    if (smoke) {
        if (!status_ok("poll_event_loop", api->poll_event_loop(runtime, 1))) {
            (void)shutdown_runtime(api, runtime);
            return EXIT_FAILURE;
        }
    } else {
        if (api->set_surface_visible == nullptr) {
            std::fprintf(stderr, "orna-runtime-qt-demo: runtime cannot show a surface\n");
            (void)shutdown_runtime(api, runtime);
            return EXIT_FAILURE;
        }
        if (!status_ok("set_surface_visible", api->set_surface_visible(runtime, surface, 1))) {
            (void)shutdown_runtime(api, runtime);
            return EXIT_FAILURE;
        }
        while (g_demo_state.surface_closed_events == 0) {
            if (!status_ok("poll_event_loop", api->poll_event_loop(runtime, 50))) {
                (void)shutdown_runtime(api, runtime);
                return EXIT_FAILURE;
            }
        }
    }

    return shutdown_runtime(api, runtime) ? EXIT_SUCCESS : EXIT_FAILURE;
}

} // namespace

int main(int argc, char **argv) {
    if (argc > 2 || (argc == 2 && std::strcmp(argv[1], "--smoke") != 0)) {
        std::fprintf(stderr, "usage: %s [--smoke]\n", argc > 0 ? argv[0] : "runtime_demo");
        return EXIT_FAILURE;
    }
    return run_demo(argc == 2);
}
