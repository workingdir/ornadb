#include <orna_runtime_abi_v1.h>

#include <QApplication>
#include <QPushButton>
#include <vector>

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
std::string string_value(OrnaStringView value) {
    return std::string(value.data == nullptr ? "" : value.data, value.len);
}

struct EventState {
    std::vector<OrnaSurfaceHandle> closed_surfaces;
    std::size_t callbacks = 0;
    std::size_t surface_closed = 0;
    std::size_t surface_close_attempts = 0;
    bool reject_surface_closed = false;
    bool enforce_queue_capacity = false;
    std::size_t queued_callbacks = 0;
    std::size_t callback_queue_capacity = 1;
    std::size_t actions = 0;
    std::string action_input_type;
    OrnaSurfaceHandle action_surface = 0;
    OrnaNodeHandle action_node = 0;
    OrnaActionHandle action_handle = 0;
    const OrnaRuntimeApiV1 *api = nullptr;
    OrnaRuntimeHandle runtime = 0;
    OrnaSurfaceHandle reentry_surface = 0;
    bool reenter_on_action = false;
    OrnaStatusCode reentry_status = ORNA_STATUS_INTERNAL;
};

OrnaStatus emit_event(void *context, OrnaRuntimeHandle, const OrnaRuntimeEventV1 *event) {
    auto *state = static_cast<EventState *>(context);
    if (state != nullptr && event != nullptr) {
        ++state->callbacks;
        if (event->kind == ORNA_RUNTIME_EVENT_SURFACE_CLOSED) {
            ++state->surface_close_attempts;
            if (state->reject_surface_closed) {
                return OrnaStatus{ORNA_STATUS_INTERNAL, view("test callback rejected close")};
            }
            if (state->enforce_queue_capacity
                && state->queued_callbacks >= state->callback_queue_capacity) {
                return OrnaStatus{ORNA_STATUS_INTERNAL, view("test callback queue is full")};
            }
            if (state->enforce_queue_capacity) {
                ++state->queued_callbacks;
            }
            ++state->surface_closed;
            state->closed_surfaces.push_back(event->as.surface_closed.surface);
        } else if (event->kind == ORNA_RUNTIME_EVENT_ACTION) {
            ++state->actions;
            state->action_surface = event->as.action.surface;
            state->action_node = event->as.action.node;
            state->action_handle = event->as.action.action;
            state->action_input_type.assign(event->as.action.payload.type_name.data,
                                            event->as.action.payload.type_name.len);
            if (state->reenter_on_action) {
                state->reentry_status =
                    state->api->set_surface_visible(state->runtime, state->reentry_surface, 1).code;
                state->reenter_on_action = false;
            }
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
    REQUIRE(string_value(descriptor->runtime_name) == "orna-runtime-qt");
    REQUIRE(string_value(descriptor->runtime_version) == "1.0.0");
    REQUIRE(string_value(descriptor->build_id) == "orna-runtime-qt-linux-x86_64");
    REQUIRE(string_value(descriptor->platform) == "linux-x86_64");
    REQUIRE(descriptor->features == ORNA_RUNTIME_FEATURE_MULTIPLE_WINDOWS);
    REQUIRE(descriptor->sinks != nullptr);
    REQUIRE(string_value(descriptor->sinks[0].type_name) == "std.ui.UI");
    REQUIRE(descriptor->sinks[0].media_type_count == 0);
    REQUIRE(descriptor->sinks[0].supports_streaming == 0);
    REQUIRE(descriptor->sinks[0].preference_rank == 0);
    REQUIRE(descriptor->contracts != nullptr);
    constexpr const char *expected_contracts[] = {
        "std.ui.window",
        "std.ui.text",
        "std.ui.button",
        "std.ui.panel",
        "std.ui.row",
        "std.ui.column",
        "std.ui.text_input",
        "std.ui.tabs",
    };
    for (std::size_t index = 0; index < descriptor->contract_count; ++index) {
        REQUIRE(string_value(descriptor->contracts[index].name) == expected_contracts[index]);
        REQUIRE(descriptor->contracts[index].major == 1);
        REQUIRE(descriptor->contracts[index].minor == 0);
        REQUIRE(descriptor->contracts[index].feature_count == 0);
    }

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
    REQUIRE(api->cancel_request(runtime, 1).code == ORNA_STATUS_UNSUPPORTED);

    OrnaSurfaceCreateOptionsV1 surface_options{
        view("window"),
        view("OrnaDB"),
        view("local"),
        OrnaBytesView{nullptr, 0},
    };
    OrnaSurfaceHandle surface = 0;
    REQUIRE(api->create_surface(runtime, &surface_options, &surface).code == ORNA_STATUS_OK);
    REQUIRE(surface != 0);
    events.api = api;
    events.runtime = runtime;
    events.reentry_surface = surface;

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
    const char key[] = "first";
    const OrnaValueRefV1 text_key{
        0,
        view("std.json.Value"),
        OrnaBytesView{reinterpret_cast<const std::uint8_t *>(key), sizeof(key) - 1},
    };
    OrnaMountNodeV1 text_mount{
        101,
        100,
        view("content"),
        0,
        view("std.ui.text"),
        1,
        0,
        text_key,
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
    REQUIRE(state.find("\"key\":{\"type\":\"std.json.Value\",\"value\":\"6669727374\"}") != std::string::npos);

    OrnaOwnedBytes opaque_state{nullptr, 0, nullptr, release_owned};
    REQUIRE(api->capture_opaque_state(runtime, surface, &opaque_state).code == ORNA_STATUS_UNSUPPORTED);
    REQUIRE(opaque_state.data == nullptr);
    REQUIRE(opaque_state.len == 0);
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
    const char stale_title[] = "stale update";
    OrnaUiOperationV1 stale_operation{};
    stale_operation.kind = ORNA_UI_OP_SET_PROPERTY;
    stale_operation.as.set_property = OrnaSetPropertyV1{
        100,
        view("title"),
        OrnaValueRefV1{
            0,
            view("std.text"),
            OrnaBytesView{reinterpret_cast<const std::uint8_t *>(stale_title), sizeof(stale_title) - 1},
        },
    };
    OrnaUiBatchV1 stale_late_batch{2, &stale_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, surface, &stale_late_batch).code == ORNA_STATUS_STALE_REVISION);
    std::string after_stale_late;
    REQUIRE(capture(api, runtime, surface, after_stale_late).code == ORNA_STATUS_OK);
    REQUIRE(after_stale_late == state);

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
    const char rollback_title[] = "must not commit";
    OrnaUiOperationV1 late_operations[2]{};
    late_operations[0].kind = ORNA_UI_OP_SET_PROPERTY;
    late_operations[0].as.set_property = OrnaSetPropertyV1{
        100,
        view("title"),
        OrnaValueRefV1{
            0,
            view("std.text"),
            OrnaBytesView{reinterpret_cast<const std::uint8_t *>(rollback_title), sizeof(rollback_title) - 1},
        },
    };
    late_operations[1] = invalid_operation;
    OrnaUiBatchV1 late_failure_batch{3, late_operations, 2};
    REQUIRE(api->apply_ui_batch(runtime, surface, &late_failure_batch).code == ORNA_STATUS_INVALID_ARGUMENT);
    std::string after_late_failure;
    REQUIRE(capture(api, runtime, surface, after_late_failure).code == ORNA_STATUS_OK);
    REQUIRE(after_late_failure == state);

    QPushButton *button = nullptr;
    for (QWidget *window : QApplication::topLevelWidgets()) {
        button = window->findChild<QPushButton *>();
        if (button != nullptr) {
            break;
        }
    }
    REQUIRE(button != nullptr);
    events.reenter_on_action = true;
    events.reentry_status = ORNA_STATUS_INTERNAL;
    button->click();
    REQUIRE(events.actions == 1);
    REQUIRE(events.action_surface == surface);
    REQUIRE(events.action_node == button_mount.node);
    REQUIRE(events.action_handle == action_binding.action);
    REQUIRE(events.action_input_type == "std.text");
    REQUIRE(events.reentry_status == ORNA_STATUS_BUSY);

    REQUIRE(api->set_surface_visible(runtime, surface, 1).code == ORNA_STATUS_OK);
    REQUIRE(api->poll_event_loop(runtime, 0).code == ORNA_STATUS_OK);

    OrnaSurfaceHandle second_surface = 0;
    REQUIRE(api->create_surface(runtime, &surface_options, &second_surface).code == ORNA_STATUS_OK);
    REQUIRE(second_surface != 0);
    REQUIRE(second_surface != surface);

    std::string second_initial;
    REQUIRE(capture(api, runtime, second_surface, second_initial).code == ORNA_STATUS_OK);

    OrnaMountNodeV1 foreign_node_mount = root_mount;
    OrnaUiOperationV1 foreign_node_operation{};
    foreign_node_operation.kind = ORNA_UI_OP_MOUNT_NODE;
    foreign_node_operation.as.mount_node = foreign_node_mount;
    OrnaUiBatchV1 foreign_node_batch{1, &foreign_node_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &foreign_node_batch).code
            == ORNA_STATUS_INVALID_ARGUMENT);
    std::string after_foreign_node;
    REQUIRE(capture(api, runtime, second_surface, after_foreign_node).code == ORNA_STATUS_OK);
    REQUIRE(after_foreign_node == second_initial);

    OrnaMountNodeV1 second_root_mount = root_mount;
    second_root_mount.node = 300;
    OrnaMountNodeV1 second_button_mount = button_mount;
    second_button_mount.node = 301;
    second_button_mount.ordinal = 0;
    second_button_mount.parent = second_root_mount.node;
    OrnaUiOperationV1 second_mount_operations[2]{};
    second_mount_operations[0].kind = ORNA_UI_OP_MOUNT_NODE;
    second_mount_operations[0].as.mount_node = second_root_mount;
    second_mount_operations[1].kind = ORNA_UI_OP_MOUNT_NODE;
    second_mount_operations[1].as.mount_node = second_button_mount;
    OrnaUiBatchV1 second_root_batch{1, &second_mount_operations[0], 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &second_root_batch).code == ORNA_STATUS_OK);
    OrnaUiBatchV1 second_button_batch{2, &second_mount_operations[1], 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &second_button_batch).code == ORNA_STATUS_OK);
    std::string second_structure;
    REQUIRE(capture(api, runtime, second_surface, second_structure).code == ORNA_STATUS_OK);
    REQUIRE(second_structure != second_initial);

    OrnaBindActionV1 foreign_action_binding = action_binding;
    foreign_action_binding.node = second_button_mount.node;
    OrnaUiOperationV1 foreign_action_operation{};
    foreign_action_operation.kind = ORNA_UI_OP_BIND_ACTION;
    foreign_action_operation.as.bind_action = foreign_action_binding;
    OrnaUiBatchV1 foreign_action_batch{3, &foreign_action_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &foreign_action_batch).code
            == ORNA_STATUS_INVALID_ARGUMENT);
    std::string after_foreign_action;
    REQUIRE(capture(api, runtime, second_surface, after_foreign_action).code == ORNA_STATUS_OK);
    REQUIRE(after_foreign_action == second_structure);

    OrnaBindActionV1 second_action_binding = action_binding;
    second_action_binding.node = second_button_mount.node;
    second_action_binding.action = 201;
    OrnaUiOperationV1 second_action_operation{};
    second_action_operation.kind = ORNA_UI_OP_BIND_ACTION;
    second_action_operation.as.bind_action = second_action_binding;
    OrnaUiBatchV1 second_action_batch{3, &second_action_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &second_action_batch).code == ORNA_STATUS_OK);
    std::string second_owned_state;
    REQUIRE(capture(api, runtime, second_surface, second_owned_state).code == ORNA_STATUS_OK);
    REQUIRE(second_owned_state != second_structure);
    QPushButton *second_button = nullptr;
    for (QWidget *window : QApplication::topLevelWidgets()) {
        auto *candidate = window->findChild<QPushButton *>();
        if (candidate != nullptr && candidate != button) {
            second_button = candidate;
            break;
        }
    }
    REQUIRE(second_button != nullptr);
    second_button->click();
    REQUIRE(events.actions == 2);
    REQUIRE(events.action_surface == second_surface);
    REQUIRE(events.action_node == second_button_mount.node);
    REQUIRE(events.action_handle == second_action_binding.action);
    REQUIRE(events.action_input_type == "std.text");
    OrnaMountNodeV1 candidate_mount = text_mount;
    candidate_mount.node = 400;
    candidate_mount.parent = second_root_mount.node;
    candidate_mount.ordinal = 1;
    candidate_mount.explicit_key = no_key;
    OrnaMountNodeV1 late_foreign_mount = text_mount;
    late_foreign_mount.parent = second_root_mount.node;
    late_foreign_mount.ordinal = 1;
    OrnaUiOperationV1 ownership_rollback_operations[2]{};
    ownership_rollback_operations[0].kind = ORNA_UI_OP_MOUNT_NODE;
    ownership_rollback_operations[0].as.mount_node = candidate_mount;
    ownership_rollback_operations[1].kind = ORNA_UI_OP_MOUNT_NODE;
    ownership_rollback_operations[1].as.mount_node = late_foreign_mount;
    OrnaUiBatchV1 ownership_rollback_batch{4, ownership_rollback_operations, 2};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &ownership_rollback_batch).code
            == ORNA_STATUS_INVALID_ARGUMENT);
    std::string after_ownership_rollback;
    REQUIRE(capture(api, runtime, second_surface, after_ownership_rollback).code == ORNA_STATUS_OK);
    REQUIRE(after_ownership_rollback == second_owned_state);

    OrnaUiOperationV1 candidate_operation{};
    candidate_operation.kind = ORNA_UI_OP_MOUNT_NODE;
    candidate_operation.as.mount_node = candidate_mount;
    OrnaUiBatchV1 candidate_batch{4, &candidate_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &candidate_batch).code == ORNA_STATUS_OK);
    REQUIRE(capture(api, runtime, second_surface, second_owned_state).code == ORNA_STATUS_OK);
    REQUIRE(second_owned_state != after_ownership_rollback);

    OrnaMountNodeV1 zero_node_mount = second_root_mount;
    zero_node_mount.node = 0;
    OrnaUiOperationV1 zero_node_operation{};
    zero_node_operation.kind = ORNA_UI_OP_MOUNT_NODE;
    zero_node_operation.as.mount_node = zero_node_mount;
    OrnaUiBatchV1 zero_node_batch{5, &zero_node_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &zero_node_batch).code
            == ORNA_STATUS_INVALID_ARGUMENT);
    std::string after_zero_node;
    REQUIRE(capture(api, runtime, second_surface, after_zero_node).code == ORNA_STATUS_OK);
    REQUIRE(after_zero_node == second_owned_state);

    OrnaMountNodeV1 wrong_kind_node_mount = second_root_mount;
    wrong_kind_node_mount.node = second_action_binding.action;
    OrnaUiOperationV1 wrong_kind_node_operation{};
    wrong_kind_node_operation.kind = ORNA_UI_OP_MOUNT_NODE;
    wrong_kind_node_operation.as.mount_node = wrong_kind_node_mount;
    OrnaUiBatchV1 wrong_kind_node_batch{5, &wrong_kind_node_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &wrong_kind_node_batch).code
            == ORNA_STATUS_INVALID_ARGUMENT);
    std::string after_wrong_kind_node;
    REQUIRE(capture(api, runtime, second_surface, after_wrong_kind_node).code == ORNA_STATUS_OK);
    REQUIRE(after_wrong_kind_node == second_owned_state);

    REQUIRE(api->destroy_surface(runtime, surface).code == ORNA_STATUS_OK);
    REQUIRE(events.closed_surfaces.size() == 1);
    REQUIRE(events.closed_surfaces[0] == surface);
    REQUIRE(events.surface_closed == 1);
    const auto callbacks_after_first_destroy = events.callbacks;
    REQUIRE(api->poll_event_loop(runtime, 0).code == ORNA_STATUS_OK);
    REQUIRE(events.callbacks == callbacks_after_first_destroy);

    REQUIRE(api->destroy_surface(runtime, surface).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(api->set_surface_visible(runtime, surface, 1).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(api->apply_ui_batch(runtime, surface, &empty_batch).code == ORNA_STATUS_NOT_FOUND);
    std::string destroyed_surface_state;
    REQUIRE(capture(api, runtime, surface, destroyed_surface_state).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(events.callbacks == callbacks_after_first_destroy);
    const char native_close_title[] = "Native close";
    auto native_surface_options = surface_options;
    native_surface_options.title = view(native_close_title);
    OrnaSurfaceHandle third_surface = 0;
    REQUIRE(api->create_surface(runtime, &native_surface_options, &third_surface).code == ORNA_STATUS_OK);
    REQUIRE(third_surface != 0);
    REQUIRE(third_surface != surface);
    REQUIRE(third_surface != second_surface);
    QWidget *native_window = nullptr;
    for (QWidget *window : QApplication::topLevelWidgets()) {
        if (window->windowTitle() == QString::fromUtf8(native_close_title)) {
            native_window = window;
            break;
        }
    }
    REQUIRE(native_window != nullptr);
    events.reject_surface_closed = true;
    const auto attempts_before_native_close = events.surface_close_attempts;
    REQUIRE(native_window->close());
    REQUIRE(events.surface_close_attempts == attempts_before_native_close + 1);
    REQUIRE(events.surface_closed == 1);
    REQUIRE(events.closed_surfaces.size() == 1);
    const auto callbacks_after_rejected_close = events.callbacks;
    REQUIRE(api->set_surface_visible(runtime, third_surface, 1).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(api->apply_ui_batch(runtime, third_surface, &empty_batch).code == ORNA_STATUS_NOT_FOUND);
    std::string native_closed_state;
    REQUIRE(capture(api, runtime, third_surface, native_closed_state).code == ORNA_STATUS_NOT_FOUND);
    OrnaOwnedBytes native_opaque_state{nullptr, 0, nullptr, release_owned};
    REQUIRE(api->capture_opaque_state(runtime, third_surface, &native_opaque_state).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(native_opaque_state.data == nullptr);
    REQUIRE(native_opaque_state.len == 0);
    REQUIRE(api->destroy_surface(runtime, third_surface).code == ORNA_STATUS_INTERNAL);
    REQUIRE(events.surface_close_attempts == attempts_before_native_close + 2);
    REQUIRE(events.callbacks == callbacks_after_rejected_close + 1);

    events.reject_surface_closed = false;
    REQUIRE(api->poll_event_loop(runtime, 0).code == ORNA_STATUS_OK);
    REQUIRE(events.surface_closed == 2);
    REQUIRE(events.closed_surfaces.size() == 2);
    REQUIRE(events.closed_surfaces[1] == third_surface);
    REQUIRE(api->destroy_surface(runtime, third_surface).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(api->set_surface_visible(runtime, third_surface, 1).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(api->apply_ui_batch(runtime, third_surface, &empty_batch).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(capture(api, runtime, third_surface, native_closed_state).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(api->capture_opaque_state(runtime, third_surface, &native_opaque_state).code == ORNA_STATUS_NOT_FOUND);
    REQUIRE(api->destroy_surface(runtime, third_surface).code == ORNA_STATUS_NOT_FOUND);

    OrnaMountNodeV1 retired_node_mount = root_mount;
    OrnaUiOperationV1 retired_node_mount_operation{};
    retired_node_mount_operation.kind = ORNA_UI_OP_MOUNT_NODE;
    retired_node_mount_operation.as.mount_node = retired_node_mount;
    OrnaUiBatchV1 retired_node_mount_batch{5, &retired_node_mount_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &retired_node_mount_batch).code
            == ORNA_STATUS_INVALID_ARGUMENT);
    std::string after_retired_node_mount;
    REQUIRE(capture(api, runtime, second_surface, after_retired_node_mount).code == ORNA_STATUS_OK);
    REQUIRE(after_retired_node_mount == second_owned_state);

    OrnaUiOperationV1 retired_node_unmount_operation{};
    retired_node_unmount_operation.kind = ORNA_UI_OP_UNMOUNT_NODE;
    retired_node_unmount_operation.as.unmount_node = root_mount.node;
    OrnaUiBatchV1 retired_node_unmount_batch{5, &retired_node_unmount_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &retired_node_unmount_batch).code
            == ORNA_STATUS_NOT_FOUND);
    std::string after_retired_node_unmount;
    REQUIRE(capture(api, runtime, second_surface, after_retired_node_unmount).code == ORNA_STATUS_OK);
    REQUIRE(after_retired_node_unmount == second_owned_state);

    OrnaBindActionV1 retired_action_binding = second_action_binding;
    retired_action_binding.action = action_binding.action;
    OrnaUiOperationV1 retired_action_bind_operation{};
    retired_action_bind_operation.kind = ORNA_UI_OP_BIND_ACTION;
    retired_action_bind_operation.as.bind_action = retired_action_binding;
    OrnaUiBatchV1 retired_action_bind_batch{5, &retired_action_bind_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &retired_action_bind_batch).code
            == ORNA_STATUS_INVALID_ARGUMENT);
    std::string after_retired_action_bind;
    REQUIRE(capture(api, runtime, second_surface, after_retired_action_bind).code == ORNA_STATUS_OK);
    REQUIRE(after_retired_action_bind == second_owned_state);

    OrnaUiOperationV1 retired_action_unbind_operation{};
    retired_action_unbind_operation.kind = ORNA_UI_OP_UNBIND_ACTION;
    retired_action_unbind_operation.as.bind_action = retired_action_binding;
    OrnaUiBatchV1 retired_action_unbind_batch{5, &retired_action_unbind_operation, 1};
    REQUIRE(api->apply_ui_batch(runtime, second_surface, &retired_action_unbind_batch).code
            == ORNA_STATUS_NOT_FOUND);
    std::string after_retired_action_unbind;
    REQUIRE(capture(api, runtime, second_surface, after_retired_action_unbind).code == ORNA_STATUS_OK);
    REQUIRE(after_retired_action_unbind == second_owned_state);

    // A bounded client event queue is pre-filled before shutdown. The
    // provider must report failure, then succeed after the caller drains it.
    const auto callbacks_before_failed_shutdown = events.callbacks;
    const auto close_attempts_before_failed_shutdown = events.surface_close_attempts;
    events.enforce_queue_capacity = true;
    events.queued_callbacks = events.callback_queue_capacity;
    REQUIRE(api->request_shutdown(runtime).code == ORNA_STATUS_INTERNAL);
    REQUIRE(events.surface_close_attempts == close_attempts_before_failed_shutdown + 1);
    REQUIRE(events.callbacks == callbacks_before_failed_shutdown + 1);
    REQUIRE(events.surface_closed == 2);

    events.queued_callbacks = 0;
    REQUIRE(api->request_shutdown(runtime).code == ORNA_STATUS_OK);
    REQUIRE(events.surface_closed == 3);
    REQUIRE(events.queued_callbacks == 1);
    REQUIRE(events.closed_surfaces.size() == 3);
    REQUIRE(events.closed_surfaces[2] == second_surface);
    const auto callbacks_after_shutdown = events.callbacks;
    REQUIRE(api->request_shutdown(runtime).code == ORNA_STATUS_OK);
    REQUIRE(events.callbacks == callbacks_after_shutdown);
    REQUIRE(api->poll_event_loop(runtime, 0).code == ORNA_STATUS_FAILED);
    REQUIRE(events.callbacks == callbacks_after_shutdown);

    REQUIRE(api->create_surface(runtime, &surface_options, &second_surface).code == ORNA_STATUS_FAILED);
    REQUIRE(second_surface == 0);
    api->destroy(runtime);
    REQUIRE(events.callbacks == callbacks_after_shutdown);
    REQUIRE(api->poll_event_loop(runtime, 0).code == ORNA_STATUS_INVALID_ARGUMENT);
    REQUIRE(events.callbacks == callbacks_after_shutdown);
    return 0;
}
