#include <orna_runtime_abi_v1.h>

#include <QApplication>
#include <QPushButton>

#include <cstddef>
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <string>

namespace {

[[noreturn]] void fail(const char *expression, int line) {
    std::fprintf(stderr, "Qt runtime contract assertion failed at line %d: %s\n", line, expression);
    std::exit(EXIT_FAILURE);
}

#define REQUIRE(condition) \
    do { \
        if (!(condition)) { \
            fail(#condition, __LINE__); \
        } \
    } while (false)

OrnaStringView view(const char *value) {
    return OrnaStringView{value, std::strlen(value)};
}

struct EventState {
    std::size_t surface_closed = 0;
    std::size_t actions = 0;
    std::string action_input_type;
};

OrnaStatus emit_event(void *context, OrnaRuntimeHandle, const OrnaRuntimeEventV1 *event) {
    auto *state = static_cast<EventState *>(context);
    if (state != nullptr && event != nullptr) {
        if (event->kind == ORNA_RUNTIME_EVENT_SURFACE_CLOSED) {
            ++state->surface_closed;
        } else if (event->kind == ORNA_RUNTIME_EVENT_ACTION) {
            ++state->actions;
            state->action_input_type.assign(event->as.action.payload.type_name.data,
                                            event->as.action.payload.type_name.len);
        }
    }
    return OrnaStatus{ORNA_STATUS_OK, OrnaStringView{nullptr, 0}};
}

OrnaStatus fail_model(void *, OrnaRequestHandle, OrnaStatus failure) {
    return failure;
}

void release_owned(void *, std::uint8_t *data, std::size_t) {
    std::free(data);
}

OrnaStatus capture(const OrnaRuntimeApiV1 *api,
                   OrnaRuntimeHandle runtime,
                   OrnaSurfaceHandle surface,
                   std::string &output) {
    OrnaOwnedBytes bytes{nullptr, 0, nullptr, release_owned};
    const auto status = api->capture_semantic_state(runtime, surface, &bytes);
    if (status.code != ORNA_STATUS_OK) {
        return status;
    }
    if (bytes.len == 0) {
        output.clear();
    } else {
        output.assign(reinterpret_cast<const char *>(bytes.data), bytes.len);
    }
    bytes.release(bytes.owner, bytes.data, bytes.len);
    return status;
}

} // namespace

int main() {
    const auto *api = orna_runtime_query_v1();
    REQUIRE(api != nullptr);
    REQUIRE(api->abi_major == 1);
    REQUIRE(api->abi_minor == 0);
    REQUIRE(api->describe != nullptr);
    const auto *descriptor = api->describe();
    REQUIRE(descriptor != nullptr);
    REQUIRE(descriptor->thread_model == ORNA_THREAD_MODEL_CALLER_PUMPS);
    REQUIRE(descriptor->sink_count == 1);
    REQUIRE(descriptor->contract_count == 8);
    REQUIRE(std::string(descriptor->contracts[0].name.data, descriptor->contracts[0].name.len) == "std.ui.window");
    REQUIRE(descriptor->contracts[0].major == 1);
    REQUIRE(descriptor->contracts[0].minor == 0);

    EventState events;
    OrnaClientApiV1 client{
        1,
        0,
        &events,
        nullptr,
        emit_event,
        nullptr,
        fail_model,
        nullptr,
        nullptr,
        nullptr,
    };
    OrnaRuntimeCreateOptionsV1 create_options{
        &client,
        view("en-GB"),
        view("UTC"),
        view("light"),
        OrnaStringView{nullptr, 0},
        OrnaStringView{nullptr, 0},
    };
    const char invalid_locale_bytes[] = {static_cast<char>(0xc3), static_cast<char>(0x28)};
    auto invalid_options = create_options;
    invalid_options.locale = OrnaStringView{invalid_locale_bytes, sizeof(invalid_locale_bytes)};
    OrnaRuntimeHandle rejected_runtime = 0;
    REQUIRE(api->create(&invalid_options, &rejected_runtime).code == ORNA_STATUS_INVALID_ARGUMENT);
    REQUIRE(rejected_runtime == 0);

    OrnaRuntimeHandle runtime = 0;
    REQUIRE(api->create(&create_options, &runtime).code == ORNA_STATUS_OK);
    REQUIRE(runtime != 0);

    OrnaSurfaceCreateOptionsV1 surface_options{
        view("window"),
        view("OrnaDB"),
        view("local"),
        OrnaBytesView{nullptr, 0},
    };
    OrnaSurfaceHandle surface = 0;
    REQUIRE(api->create_surface(runtime, &surface_options, &surface).code == ORNA_STATUS_OK);
    REQUIRE(surface != 0);

    const OrnaValueRefV1 no_key{0, OrnaStringView{nullptr, 0}, OrnaBytesView{nullptr, 0}};
    OrnaMountNodeV1 root_mount{
        100,
        0,
        view("root"),
        0,
        view("std.ui.window"),
        1,
        0,
        no_key,
    };
    OrnaMountNodeV1 text_mount{
        101,
        100,
        view("content"),
        0,
        view("std.ui.text"),
        1,
        0,
        no_key,
    };
    OrnaMountNodeV1 button_mount{
        102,
        100,
        view("content"),
        1,
        view("std.ui.button"),
        1,
        0,
        no_key,
    };
    const char text[] = "Hello from Qt";
    OrnaSetPropertyV1 text_property{
        101,
        view("text"),
        OrnaValueRefV1{
            0,
            view("std.text"),
            OrnaBytesView{reinterpret_cast<const std::uint8_t *>(text), sizeof(text) - 1},
        },
    };
    OrnaBindActionV1 action_binding{
        102,
        view("clicked"),
        200,
        view("std.text"),
    };
    OrnaUiOperationV1 operations[5]{};
    operations[0].kind = ORNA_UI_OP_MOUNT_NODE;
    operations[0].as.mount_node = root_mount;
    operations[1].kind = ORNA_UI_OP_MOUNT_NODE;
    operations[1].as.mount_node = text_mount;
    operations[2].kind = ORNA_UI_OP_SET_PROPERTY;
    operations[2].as.set_property = text_property;
    operations[3].kind = ORNA_UI_OP_MOUNT_NODE;
    operations[3].as.mount_node = button_mount;
    operations[4].kind = ORNA_UI_OP_BIND_ACTION;
    operations[4].as.bind_action = action_binding;
    OrnaUiBatchV1 batch{1, operations, 5};
    REQUIRE(api->apply_ui_batch(runtime, surface, &batch).code == ORNA_STATUS_OK);

    std::string state;
    REQUIRE(capture(api, runtime, surface, state).code == ORNA_STATUS_OK);
    REQUIRE(state.rfind("ORNA-UI/1 ", 0) == 0);
    REQUIRE(state.find("48656c6c6f2066726f6d205174") != std::string::npos);

    OrnaUiBatchV1 stale{1, operations, 0};
    REQUIRE(api->apply_ui_batch(runtime, surface, &stale).code == ORNA_STATUS_STALE_REVISION);

    OrnaMountNodeV1 transient_mount = text_mount;
    transient_mount.node = 103;
    OrnaUiOperationV1 transient_operations[2]{};
    transient_operations[0].kind = ORNA_UI_OP_MOUNT_NODE;
    transient_operations[0].as.mount_node = transient_mount;
    transient_operations[1].kind = ORNA_UI_OP_UNMOUNT_NODE;
    transient_operations[1].as.unmount_node = transient_mount.node;
    OrnaUiBatchV1 transient_batch{2, transient_operations, 2};
    REQUIRE(api->apply_ui_batch(runtime, surface, &transient_batch).code == ORNA_STATUS_OK);
    std::string after_transient;
    REQUIRE(capture(api, runtime, surface, after_transient).code == ORNA_STATUS_OK);
    REQUIRE(after_transient == state);

    OrnaUiOperationV1 reused_operation{};
    reused_operation.kind = ORNA_UI_OP_MOUNT_NODE;
    reused_operation.as.mount_node = transient_mount;
    OrnaUiBatchV1 reused_batch{3, &reused_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, surface, &reused_batch).code == ORNA_STATUS_INVALID_ARGUMENT);

    OrnaUiBatchV1 empty_batch{3, nullptr, 0};
    REQUIRE(api->apply_ui_batch(runtime, surface, &empty_batch).code == ORNA_STATUS_INVALID_ARGUMENT);

    std::uint8_t boolean_byte = 1;
    OrnaUiOperationV1 wrong_property_operation{};
    wrong_property_operation.kind = ORNA_UI_OP_SET_PROPERTY;
    wrong_property_operation.as.set_property = OrnaSetPropertyV1{
        101,
        view("text"),
        OrnaValueRefV1{
            0,
            view("std.boolean"),
            OrnaBytesView{&boolean_byte, sizeof(boolean_byte)},
        },
    };
    OrnaUiBatchV1 wrong_property_batch{3, &wrong_property_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, surface, &wrong_property_batch).code == ORNA_STATUS_INVALID_ARGUMENT);

    OrnaUiOperationV1 unknown_operation{};
    unknown_operation.kind = ORNA_UI_OP_MOUNT_NODE;
    unknown_operation.as.mount_node = transient_mount;
    unknown_operation.as.mount_node.node = 105;
    unknown_operation.as.mount_node.contract_name = view("std.ui.unknown");
    OrnaUiBatchV1 unknown_batch{3, &unknown_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, surface, &unknown_batch).code == ORNA_STATUS_UNSUPPORTED);

    OrnaMountNodeV1 wrong_version = root_mount;
    wrong_version.node = 106;
    wrong_version.contract_minor = 1;
    OrnaUiOperationV1 wrong_version_operation{};
    wrong_version_operation.kind = ORNA_UI_OP_MOUNT_NODE;
    wrong_version_operation.as.mount_node = wrong_version;
    OrnaUiBatchV1 wrong_version_batch{3, &wrong_version_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, surface, &wrong_version_batch).code == ORNA_STATUS_UNSUPPORTED);

    OrnaUiOperationV1 unsupported_operation{};
    unsupported_operation.kind = ORNA_UI_OP_SET_FOCUS;
    OrnaUiBatchV1 unsupported_batch{3, &unsupported_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, surface, &unsupported_batch).code == ORNA_STATUS_UNSUPPORTED);

    OrnaUiOperationV1 invalid_operation{};
    invalid_operation.kind = ORNA_UI_OP_SET_PROPERTY;
    invalid_operation.as.set_property = OrnaSetPropertyV1{
        101,
        view("unknown"),
        OrnaValueRefV1{0, view("std.text"), OrnaBytesView{nullptr, 0}},
    };
    OrnaUiBatchV1 invalid_batch{3, &invalid_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, surface, &invalid_batch).code == ORNA_STATUS_INVALID_ARGUMENT);
    std::string unchanged;
    REQUIRE(capture(api, runtime, surface, unchanged).code == ORNA_STATUS_OK);
    REQUIRE(unchanged == state);

    QPushButton *button = nullptr;
    for (QWidget *window : QApplication::topLevelWidgets()) {
        button = window->findChild<QPushButton *>();
        if (button != nullptr) {
            break;
        }
    }
    REQUIRE(button != nullptr);
    button->click();
    REQUIRE(events.actions == 1);
    REQUIRE(events.action_input_type == "std.text");

    REQUIRE(api->set_surface_visible(runtime, surface, 1).code == ORNA_STATUS_OK);
    REQUIRE(api->poll_event_loop(runtime, 0).code == ORNA_STATUS_OK);
    REQUIRE(api->destroy_surface(runtime, surface).code == ORNA_STATUS_OK);
    REQUIRE(events.surface_closed == 1);

    OrnaSurfaceHandle second_surface = 0;
    REQUIRE(api->create_surface(runtime, &surface_options, &second_surface).code == ORNA_STATUS_OK);
    REQUIRE(api->request_shutdown(runtime).code == ORNA_STATUS_OK);
    REQUIRE(events.surface_closed == 2);
    REQUIRE(api->create_surface(runtime, &surface_options, &second_surface).code == ORNA_STATUS_FAILED);
    api->destroy(runtime);
    REQUIRE(api->poll_event_loop(runtime, 0).code == ORNA_STATUS_INVALID_ARGUMENT);
    return 0;
}
