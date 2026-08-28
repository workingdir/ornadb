#include <orna_runtime_abi_v1.h>

#include <QApplication>
#include <QByteArray>
#include <QCloseEvent>
#include <QCoreApplication>
#include <QFrame>
#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QMetaObject>
#include <QPointer>
#include <QPushButton>
#include <QThread>
#include <QVBoxLayout>
#include <QWidget>

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <memory>
#include <utility>
#include <new>
#include <stdexcept>
#include <thread>
#include <unordered_map>
#include <unordered_set>
#include <vector>

namespace {

constexpr std::size_t kMaxBytes = 16U * 1024U * 1024U;
constexpr std::size_t kMaxDepth = 128;
constexpr std::size_t kMaxOperations = 1024;
constexpr std::size_t kMaxText = 4096;
constexpr std::size_t kMaxNodes = 4096;

const char kRuntimeName[] = "orna-runtime-qt";
const char kRuntimeVersion[] = "1.0.0";
const char kBuildId[] = "orna-runtime-qt-linux-x86_64";
const char kPlatform[] = "linux-x86_64";
const char kUiType[] = "std.ui.UI";
const char kUnsupported[] = "operation is not supported by the Qt v1 runtime";
const char kInvalid[] = "invalid Qt v1 runtime input";
const char kShutdown[] = "runtime is shut down";
const char kDraining[] = "runtime is draining";
const char kStale[] = "semantic revision is stale";
const char kNotFound[] = "runtime object was not found";
const char kBusy[] = "runtime callback re-entry is not allowed";
const char kInternal[] = "Qt runtime invariant failed";

constexpr const char *kContracts[] = {
    "std.ui.window",
    "std.ui.text",
    "std.ui.button",
    "std.ui.panel",
    "std.ui.row",
    "std.ui.column",
    "std.ui.text_input",
    "std.ui.tabs",
};

struct Runtime;

QApplication *g_application = nullptr;
bool g_application_owned = false;
bool g_restore_quit_on_last_window_closed = false;
bool g_saved_quit_on_last_window_closed = true;
std::size_t g_runtime_count = 0;
std::thread::id g_application_thread;
std::uint64_t g_next_handle = 1;
std::unordered_map<OrnaRuntimeHandle, Runtime *> g_live_runtimes;

OrnaStringView string_view(const char *value) {
    return OrnaStringView{value, std::strlen(value)};
}

OrnaStatus result(OrnaStatusCode code, const char *message) {
    return OrnaStatus{code, string_view(message)};
}

OrnaStatus ok() {
    return result(ORNA_STATUS_OK, "");
}

OrnaStatus invalid() {
    return result(ORNA_STATUS_INVALID_ARGUMENT, kInvalid);
}

OrnaStatus unsupported() {
    return result(ORNA_STATUS_UNSUPPORTED, kUnsupported);
}

OrnaStatus not_found() {
    return result(ORNA_STATUS_NOT_FOUND, kNotFound);
}

OrnaStatus failed(const char *message) {
    return result(ORNA_STATUS_FAILED, message);
}

OrnaStatus internal_error() {
    return result(ORNA_STATUS_INTERNAL, kInternal);
}

bool valid_string_view(OrnaStringView value, std::size_t limit = kMaxText) {
    if (value.len > limit || (value.len > 0 && value.data == nullptr)) {
        return false;
    }
    if (value.len == 0) {
        return true;
    }
    const QByteArray bytes(value.data, static_cast<qsizetype>(value.len));
    const QString decoded = QString::fromUtf8(bytes);
    return decoded.toUtf8() == bytes;
}

bool valid_bytes_view(OrnaBytesView value, std::size_t limit = kMaxBytes) {
    return value.len <= limit && (value.len == 0 || value.data != nullptr);
}

QString read_string(OrnaStringView value) {
    if (!valid_string_view(value)) {
        return {};
    }
    return QString::fromUtf8(value.data, static_cast<qsizetype>(value.len));
}

QByteArray read_bytes(OrnaBytesView value) {
    if (!valid_bytes_view(value)) {
        return {};
    }
    return QByteArray(reinterpret_cast<const char *>(value.data), static_cast<qsizetype>(value.len));
}

bool is_allowed_contract(const QString &contract) {
    for (const char *candidate : kContracts) {
        if (contract == QString::fromLatin1(candidate)) {
            return true;
        }
    }
    return false;
}

QString operation_contract(const OrnaStringView &value) {
    return read_string(value);
}

bool valid_handle(OrnaHandle handle) {
    return handle != 0;
}

struct ActionBinding {
    OrnaActionHandle handle = 0;
    QString input_type;
    QMetaObject::Connection connection;
};

struct Node {
    OrnaNodeHandle handle = 0;
    OrnaNodeHandle parent = 0;
    QString contract;
    QString slot;
    QPointer<QWidget> widget;
    std::vector<OrnaNodeHandle> children;
    std::map<std::string, QByteArray> properties;
    std::map<std::string, QString> property_types;
    std::map<std::string, ActionBinding> actions;
    bool has_explicit_key = false;
    QString explicit_key_type;
    QByteArray explicit_key_bytes;
};

struct Surface {
    OrnaSurfaceHandle handle = 0;
    QPointer<QWidget> window;
    QString base_title;
    std::uint64_t semantic_revision = 0;
    bool visible = false;
    bool native_closed = false;
    bool close_event_delivered = false;
    std::map<OrnaNodeHandle, Node> nodes;
};

struct ProjectedAction {
    OrnaActionHandle handle = 0;
    QString input_type;
};

struct ProjectedNode {
    OrnaNodeHandle handle = 0;
    OrnaNodeHandle parent = 0;
    QString contract;
    QString slot;
    std::vector<OrnaNodeHandle> children;
    std::map<std::string, QByteArray> properties;
    std::map<std::string, QString> property_types;
    std::map<std::string, ProjectedAction> actions;
    bool has_explicit_key = false;
    QString explicit_key_type;
    QByteArray explicit_key_bytes;
};

struct Runtime {
    OrnaRuntimeHandle handle = 0;
    OrnaClientApiV1 client{};
    std::thread::id owner_thread;
    std::map<OrnaSurfaceHandle, Surface> surfaces;
    std::unordered_set<OrnaNodeHandle> live_node_handles;
    std::unordered_set<OrnaNodeHandle> retired_node_handles;
    std::unordered_set<OrnaActionHandle> live_action_handles;
    std::unordered_set<OrnaActionHandle> retired_action_handles;
    bool draining = false;
    bool terminal = false;
    bool in_callback = false;
};

bool emit_surface_closed(Runtime *runtime, OrnaSurfaceHandle surface);

class RuntimeWindow final : public QWidget {
public:
    RuntimeWindow(Runtime *runtime, OrnaSurfaceHandle surface)
        : runtime_(runtime), surface_(surface) {}

    void suppress_close_event() {
        suppress_close_event_ = true;
    }

protected:
    void closeEvent(QCloseEvent *event) override;

private:
    Runtime *runtime_;
    OrnaSurfaceHandle surface_;
    bool suppress_close_event_ = false;
};
OrnaRuntimeHandle runtime_handle(const Runtime *runtime) {
    return runtime == nullptr ? 0 : runtime->handle;
}

Runtime *runtime_from_handle(OrnaRuntimeHandle handle) {
    const auto found = g_live_runtimes.find(handle);
    return found == g_live_runtimes.end() ? nullptr : found->second;
}

void release_application_after_failed_create(bool created_application) {
    if (g_application == nullptr) {
        return;
    }
    if (created_application) {
        delete g_application;
        g_application = nullptr;
        g_application_owned = false;
        g_application_thread = {};
        return;
    }
    if (g_restore_quit_on_last_window_closed) {
        g_application->setQuitOnLastWindowClosed(g_saved_quit_on_last_window_closed);
        g_restore_quit_on_last_window_closed = false;
    }
    g_application = nullptr;
    g_application_thread = {};
}
OrnaHandle next_handle() {
    if (g_next_handle == std::numeric_limits<OrnaHandle>::max()) {
        return 0;
    }
    return g_next_handle++;
}

bool on_owner_thread(const Runtime *runtime) {
    return runtime != nullptr && runtime->owner_thread == std::this_thread::get_id();
}

OrnaStatus operational(Runtime *runtime) {
    if (runtime == nullptr || !on_owner_thread(runtime)) {
        return invalid();
    }
    if (runtime->in_callback) {
        return result(ORNA_STATUS_BUSY, kBusy);
    }
    if (runtime->terminal) {
        return failed(kShutdown);
    }
    if (runtime->draining) {
        return failed(kDraining);
    }
    return ok();
}

OrnaStatus draining_or_terminal(Runtime *runtime) {
    if (runtime == nullptr || !on_owner_thread(runtime)) {
        return invalid();
    }
    if (runtime->in_callback) {
        return result(ORNA_STATUS_BUSY, kBusy);
    }
    return ok();
}

void release_owned(void *, std::uint8_t *data, std::size_t) {
    std::free(data);
}

OrnaStatus owned_bytes(const QByteArray &bytes, OrnaOwnedBytes *output) {
    if (output == nullptr || bytes.size() < 0 || static_cast<std::size_t>(bytes.size()) > kMaxBytes) {
        return invalid();
    }
    output->data = nullptr;
    output->len = 0;
    output->owner = nullptr;
    output->release = release_owned;
    if (bytes.isEmpty()) {
        return ok();
    }
    auto *data = static_cast<std::uint8_t *>(std::malloc(static_cast<std::size_t>(bytes.size())));
    if (data == nullptr) {
        return internal_error();
    }
    std::memcpy(data, bytes.constData(), static_cast<std::size_t>(bytes.size()));
    output->data = data;
    output->len = static_cast<std::size_t>(bytes.size());
    return ok();
}

QLayout *ensure_layout(QWidget *parent, const QString &contract) {
    if (parent == nullptr) {
        return nullptr;
    }
    if (parent->layout() != nullptr) {
        return parent->layout();
    }
    if (contract == QLatin1String("std.ui.row")) {
        return new QHBoxLayout(parent);
    }
    return new QVBoxLayout(parent);
}

QWidget *create_widget(Surface &surface, const QString &contract) {
    if (contract == QLatin1String("std.ui.window")) {
        return surface.window;
    }
    if (contract == QLatin1String("std.ui.text")) {
        return new QLabel(surface.window);
    }
    if (contract == QLatin1String("std.ui.button")) {
        return new QPushButton(surface.window);
    }
    if (contract == QLatin1String("std.ui.text_input")) {
        return new QLineEdit(surface.window);
    }
    if (contract == QLatin1String("std.ui.tabs")) {
        return new QFrame(surface.window);
    }
    auto *container = new QFrame(surface.window);
    container->setFrameShape(QFrame::NoFrame);
    return container;
}


bool contains_child(const std::vector<OrnaNodeHandle> &children, OrnaNodeHandle handle) {
    return std::find(children.begin(), children.end(), handle) != children.end();
}

void remove_child_reference(std::vector<OrnaNodeHandle> &children, OrnaNodeHandle handle) {
    children.erase(std::remove(children.begin(), children.end(), handle), children.end());
}

bool property_allowed(const QString &contract, const std::string &property) {
    if (contract == QLatin1String("std.ui.window")) {
        return property == "title";
    }
    if (contract == QLatin1String("std.ui.text")) {
        return property == "text";
    }
    if (contract == QLatin1String("std.ui.button")) {
        return property == "label" || property == "enabled";
    }
    if (contract == QLatin1String("std.ui.text_input")) {
        return property == "text" || property == "placeholder" || property == "enabled";
    }
    return false;
}
bool accepts_children(const QString &contract, const QString &slot) {
    if (slot != QLatin1String("content")) {
        return false;
    }
    return contract == QLatin1String("std.ui.window")
        || contract == QLatin1String("std.ui.panel")
        || contract == QLatin1String("std.ui.row")
        || contract == QLatin1String("std.ui.column")
        || contract == QLatin1String("std.ui.tabs");
}

bool valid_property_name(OrnaStringView property) {
    return valid_string_view(property) && property.len > 0;
}

bool has_ancestor(const std::unordered_map<OrnaNodeHandle, ProjectedNode> &nodes,
                  OrnaNodeHandle node,
                  OrnaNodeHandle candidate) {
    auto current = node;
    while (current != 0) {
        if (current == candidate) {
            return true;
        }
        const auto found = nodes.find(current);
        if (found == nodes.end()) {
            return false;
        }
        current = found->second.parent;
    }
    return false;
}

bool projected_action_exists(const std::unordered_map<OrnaNodeHandle, ProjectedNode> &nodes,
                             OrnaActionHandle action) {
    for (const auto &[node_handle, node] : nodes) {
        (void)node_handle;
        for (const auto &[event, binding] : node.actions) {
            (void)event;
            if (binding.handle == action) {
                return true;
            }
        }
    }
    return false;
}

bool project_nodes(const Surface &surface, std::unordered_map<OrnaNodeHandle, ProjectedNode> &projected) {
    if (surface.nodes.size() > kMaxNodes) {
        return false;
    }
    try {
        projected.reserve(surface.nodes.size() + 8);
        for (const auto &[handle, node] : surface.nodes) {
            ProjectedNode copy;
            copy.handle = handle;
            copy.parent = node.parent;
            copy.contract = node.contract;
            copy.slot = node.slot;
            copy.children = node.children;
            copy.properties = node.properties;
            copy.property_types = node.property_types;
            copy.has_explicit_key = node.has_explicit_key;
            copy.explicit_key_type = node.explicit_key_type;
            copy.explicit_key_bytes = node.explicit_key_bytes;
            for (const auto &[event, binding] : node.actions) {
                copy.actions.emplace(event, ProjectedAction{binding.handle, binding.input_type});
            }
            projected.emplace(handle, std::move(copy));
        }
    } catch (const std::bad_alloc &) {
        return false;
    }
    return true;
}
bool valid_depth(const std::unordered_map<OrnaNodeHandle, ProjectedNode> &nodes) {
    std::vector<std::pair<OrnaNodeHandle, std::size_t>> pending;
    try {
        pending.reserve(nodes.size());
        for (const auto &[handle, node] : nodes) {
            if (node.parent == 0) {
                pending.emplace_back(handle, 0);
            }
        }
        while (!pending.empty()) {
            const auto [handle, depth] = pending.back();
            pending.pop_back();
            if (depth > kMaxDepth) {
                return false;
            }
            const auto node = nodes.find(handle);
            if (node == nodes.end()) {
                return false;
            }
            for (const auto child : node->second.children) {
                pending.emplace_back(child, depth + 1);
            }
        }
    } catch (const std::bad_alloc &) {
        return false;
    }
    return true;
}
bool project_unmount(std::unordered_map<OrnaNodeHandle, ProjectedNode> &nodes, OrnaNodeHandle handle) {
    if (nodes.find(handle) == nodes.end()) {
        return false;
    }
    std::vector<OrnaNodeHandle> pending;
    std::vector<OrnaNodeHandle> subtree;
    try {
        pending.push_back(handle);
        while (!pending.empty()) {
            const auto current = pending.back();
            pending.pop_back();
            const auto node = nodes.find(current);
            if (node == nodes.end()) {
                return false;
            }
            subtree.push_back(current);
            for (const auto child : node->second.children) {
                pending.push_back(child);
            }
        }
        for (auto iterator = subtree.rbegin(); iterator != subtree.rend(); ++iterator) {
            const auto node = nodes.find(*iterator);
            if (node == nodes.end()) {
                return false;
            }
            if (node->second.parent != 0) {
                const auto parent = nodes.find(node->second.parent);
                if (parent == nodes.end()) {
                    return false;
                }
                remove_child_reference(parent->second.children, *iterator);
            }
            nodes.erase(node);
        }
    } catch (const std::bad_alloc &) {
        return false;
    }
    return true;
}

bool value_type_is(const OrnaValueRefV1 &value, const char *expected) {
    return read_string(value.type_name) == QString::fromLatin1(expected);
}

bool valid_text_value(const OrnaValueRefV1 &value) {
    if (!value_type_is(value, "std.text") || !valid_bytes_view(value.canonical_encoding)) {
        return false;
    }
    const auto bytes = read_bytes(value.canonical_encoding);
    return QString::fromUtf8(bytes).toUtf8() == bytes;
}

bool valid_boolean_value(const OrnaValueRefV1 &value) {
    if (!value_type_is(value, "std.boolean") || !valid_bytes_view(value.canonical_encoding)) {
        return false;
    }
    const auto bytes = read_bytes(value.canonical_encoding);
    return bytes.size() == 1 && (bytes[0] == 0 || bytes[0] == 1);
}
bool valid_optional_key(const OrnaValueRefV1 &value) {
    const bool absent = value.handle == 0 && value.type_name.len == 0 && value.canonical_encoding.len == 0;
    if (absent) {
        return value.type_name.data == nullptr && value.canonical_encoding.data == nullptr;
    }
    return valid_string_view(value.type_name) && value.type_name.len > 0
        && valid_bytes_view(value.canonical_encoding);
}

bool validate_property_value(const QString &contract, const std::string &property, const OrnaValueRefV1 &value) {
    if (!valid_string_view(value.type_name) || value.type_name.len == 0) {
        return false;
    }
    if (property == "enabled") {
        return (contract == QLatin1String("std.ui.button") || contract == QLatin1String("std.ui.text_input"))
            && valid_boolean_value(value);
    }
    const bool is_text_property =
        (property == "title" && contract == QLatin1String("std.ui.window"))
        || (property == "text" && (contract == QLatin1String("std.ui.text")
                                   || contract == QLatin1String("std.ui.text_input")))
        || (property == "label" && contract == QLatin1String("std.ui.button"))
        || (property == "placeholder" && contract == QLatin1String("std.ui.text_input"));
    return is_text_property && valid_text_value(value);
}

bool validate_batch(const Runtime &runtime,
                    const Surface &surface,
                    const OrnaUiBatchV1 &batch,
                    std::unordered_map<OrnaNodeHandle, ProjectedNode> &nodes) {
    if (batch.semantic_revision <= surface.semantic_revision || batch.operation_count == 0
        || batch.operation_count > kMaxOperations) {
        return false;
    }
    if (batch.operation_count > 0 && batch.operations == nullptr) {
        return false;
    }
    nodes.clear();
    if (!project_nodes(surface, nodes)) {
        return false;
    }
    if (!valid_depth(nodes)) {
        return false;
    }
    std::unordered_set<OrnaNodeHandle> mounted_aliases;
    std::unordered_set<OrnaActionHandle> bound_aliases;
    for (std::size_t index = 0; index < batch.operation_count; ++index) {
        const auto &operation = batch.operations[index];
        switch (operation.kind) {
        case ORNA_UI_OP_MOUNT_NODE: {
            const auto &mount = operation.as.mount_node;
            const auto contract = operation_contract(mount.contract_name);
            const auto slot = read_string(mount.slot);
            if (!valid_handle(mount.node) || nodes.find(mount.node) != nodes.end()
                || mounted_aliases.find(mount.node) != mounted_aliases.end()
                || bound_aliases.find(mount.node) != bound_aliases.end()
                || runtime.live_node_handles.find(mount.node) != runtime.live_node_handles.end()
                || runtime.retired_node_handles.find(mount.node) != runtime.retired_node_handles.end()
                || runtime.live_action_handles.find(mount.node) != runtime.live_action_handles.end()
                || runtime.retired_action_handles.find(mount.node) != runtime.retired_action_handles.end()
                || !is_allowed_contract(contract) || mount.contract_major != 1 || mount.contract_minor != 0
                || ((mount.parent == 0) != (contract == QLatin1String("std.ui.window")))
                || !valid_string_view(mount.slot) || slot.isEmpty() || !valid_optional_key(mount.explicit_key)
                || (mount.parent != 0 && nodes.find(mount.parent) == nodes.end())) {
                return false;
            }
            if (mount.parent != 0 && !accepts_children(nodes.at(mount.parent).contract, slot)) {
                return false;
            }
            if (mount.parent != 0) {
                auto &children = nodes.at(mount.parent).children;
                if (mount.ordinal > children.size()) {
                    return false;
                }
                children.insert(children.begin() + static_cast<std::ptrdiff_t>(mount.ordinal), mount.node);
            } else if (mount.ordinal != 0) {
                return false;
            }
            if ((mount.parent == 0 && !nodes.empty()) || nodes.size() >= kMaxNodes) {
                return false;
            }
            ProjectedNode node;
            node.handle = mount.node;
            node.parent = mount.parent;
            node.contract = contract;
            node.slot = slot;
            node.has_explicit_key = !(mount.explicit_key.handle == 0
                                      && mount.explicit_key.type_name.len == 0
                                      && mount.explicit_key.canonical_encoding.len == 0);
            node.explicit_key_type = read_string(mount.explicit_key.type_name);
            node.explicit_key_bytes = read_bytes(mount.explicit_key.canonical_encoding);
            nodes.emplace(mount.node, std::move(node));
            mounted_aliases.insert(mount.node);
            break;
        }
        case ORNA_UI_OP_UNMOUNT_NODE:
            if (!valid_handle(operation.as.unmount_node) || !project_unmount(nodes, operation.as.unmount_node)) {
                return false;
            }
            break;
        case ORNA_UI_OP_SET_PROPERTY:
        case ORNA_UI_OP_CLEAR_PROPERTY: {
            const auto &property = operation.as.set_property;
            if (!valid_handle(property.node) || !valid_property_name(property.property)) {
                return false;
            }
            const auto node = nodes.find(property.node);
            if (node == nodes.end()) {
                return false;
            }
            const auto name = read_string(property.property).toUtf8().toStdString();
            if (!property_allowed(node->second.contract, name)) {
                return false;
            }
            if (operation.kind == ORNA_UI_OP_SET_PROPERTY
                && !validate_property_value(node->second.contract, name, property.value)) {
                return false;
            }
            if (operation.kind == ORNA_UI_OP_SET_PROPERTY) {
                node->second.properties[name] = read_bytes(property.value.canonical_encoding);
                node->second.property_types[name] = read_string(property.value.type_name);
            } else {
                node->second.properties.erase(name);
                node->second.property_types.erase(name);
            }
            break;
        }
        case ORNA_UI_OP_INSERT_CHILD:
        case ORNA_UI_OP_REMOVE_CHILD:
        case ORNA_UI_OP_MOVE_CHILD: {
            const auto &child = operation.as.child;
            if (!valid_handle(child.parent) || !valid_handle(child.child) || !valid_string_view(child.slot)) {
                return false;
            }
            const auto parent = nodes.find(child.parent);
            const auto child_node = nodes.find(child.child);
            if (parent == nodes.end() || child_node == nodes.end() || child.parent == child.child) {
                return false;
            }
            const auto slot = read_string(child.slot);
            if (slot.isEmpty() || has_ancestor(nodes, child.parent, child.child)) {
                return false;
            }
            if (child_node->second.contract == QLatin1String("std.ui.window")
                || !accepts_children(parent->second.contract, slot)) {
                return false;
            }
            if (operation.kind == ORNA_UI_OP_INSERT_CHILD) {
                if (child_node->second.parent != 0 || child.ordinal > parent->second.children.size()) {
                    return false;
                }
                parent->second.children.insert(
                    parent->second.children.begin() + static_cast<std::ptrdiff_t>(child.ordinal), child.child);
                child_node->second.parent = child.parent;
                child_node->second.slot = slot;
            } else if (operation.kind == ORNA_UI_OP_REMOVE_CHILD) {
                if (child_node->second.parent != child.parent || child_node->second.slot != slot
                    || child.ordinal >= parent->second.children.size()
                    || parent->second.children[child.ordinal] != child.child) {
                    return false;
                }
                parent->second.children.erase(parent->second.children.begin()
                                              + static_cast<std::ptrdiff_t>(child.ordinal));
                child_node->second.parent = 0;
                child_node->second.slot.clear();
            } else {
                if (child_node->second.parent != child.parent || child_node->second.slot != slot
                    || !contains_child(parent->second.children, child.child)
                    || child.ordinal > parent->second.children.size()) {
                    return false;
                }
                remove_child_reference(parent->second.children, child.child);
                const auto ordinal = std::min(child.ordinal, parent->second.children.size());
                parent->second.children.insert(
                    parent->second.children.begin() + static_cast<std::ptrdiff_t>(ordinal), child.child);
                child_node->second.slot = slot;
            }
            break;
        }
        case ORNA_UI_OP_BIND_ACTION:
        case ORNA_UI_OP_UNBIND_ACTION: {
            const auto &binding = operation.as.bind_action;
            if (!valid_handle(binding.node) || !valid_handle(binding.action) || !valid_string_view(binding.event_name)) {
                return false;
            }
            const auto node = nodes.find(binding.node);
            const auto event = read_string(binding.event_name).toUtf8().toStdString();
            if (node == nodes.end() || event.empty()) {
                return false;
            }
            if (operation.kind == ORNA_UI_OP_BIND_ACTION) {
                if (node->second.contract != QLatin1String("std.ui.button")
                    || node->second.actions.find(event) != node->second.actions.end()
                    || bound_aliases.find(binding.action) != bound_aliases.end()
                    || mounted_aliases.find(binding.action) != mounted_aliases.end()
                    || runtime.live_action_handles.find(binding.action) != runtime.live_action_handles.end()
                    || runtime.retired_action_handles.find(binding.action) != runtime.retired_action_handles.end()
                    || runtime.live_node_handles.find(binding.action) != runtime.live_node_handles.end()
                    || runtime.retired_node_handles.find(binding.action) != runtime.retired_node_handles.end()
                    || projected_action_exists(nodes, binding.action)
                    || !valid_string_view(binding.input_type) || read_string(binding.input_type).isEmpty()) {
                    return false;
                }
                bound_aliases.insert(binding.action);
                node->second.actions.emplace(
                    event, ProjectedAction{binding.action, read_string(binding.input_type)});
            } else {
                const auto existing = node->second.actions.find(event);
                if (existing == node->second.actions.end() || existing->second.handle != binding.action) {
                    return false;
                }
                node->second.actions.erase(existing);
            }
            break;
        }
        case ORNA_UI_OP_SET_FOCUS:
        case ORNA_UI_OP_SET_ACCESSIBILITY:
            return false;
        default:
            return false;
        }
    }
    return valid_depth(nodes);
}

void set_enabled(QWidget *widget, const QByteArray &value) {
    if (widget == nullptr || value.size() != 1) {
        return;
    }
    widget->setEnabled(value[0] != 0);
}

void apply_property(Node &node,
                     const std::string &name,
                     const QString &type,
                     const QByteArray &value) {
    node.properties[name] = value;
    node.property_types[name] = type;
    auto *widget = node.widget.data();
    if (widget == nullptr) {
        return;
    }
    if (name == "title") {
        widget->setWindowTitle(QString::fromUtf8(value));
    } else if (name == "text") {
        if (auto *label = qobject_cast<QLabel *>(widget)) {
            label->setText(QString::fromUtf8(value));
        } else if (auto *input = qobject_cast<QLineEdit *>(widget)) {
            input->setText(QString::fromUtf8(value));
        }
    } else if (name == "label") {
        if (auto *button = qobject_cast<QPushButton *>(widget)) {
            button->setText(QString::fromUtf8(value));
        }
    } else if (name == "placeholder") {
        if (auto *input = qobject_cast<QLineEdit *>(widget)) {
            input->setPlaceholderText(QString::fromUtf8(value));
        }
    } else if (name == "enabled") {
        set_enabled(widget, value);
}
}



void attach_widget(Surface &surface, Node &node) {
    auto *widget = node.widget.data();
    if (widget == nullptr || node.parent == 0) {
        return;
    }
    const auto parent = surface.nodes.find(node.parent);
    if (parent == surface.nodes.end() || parent->second.widget == nullptr) {
        return;
    }
    auto *layout = ensure_layout(parent->second.widget, parent->second.contract);
    if (layout == nullptr || widget == surface.window) {
        return;
    }
    layout->addWidget(widget);
}

void detach_widget(Surface &surface, Node &node) {
    if (node.parent == 0 || node.widget == nullptr) {
        return;
    }
    const auto parent = surface.nodes.find(node.parent);
    if (parent != surface.nodes.end() && parent->second.widget != nullptr && parent->second.widget->layout() != nullptr) {
        parent->second.widget->layout()->removeWidget(node.widget);
    }
    node.widget->setParent(nullptr);
}

void emit_action(Runtime *runtime,
                 OrnaSurfaceHandle surface,
                 OrnaNodeHandle node,
                 OrnaActionHandle action,
                 const QString &input_type) {
    if (runtime == nullptr || runtime->terminal || runtime->client.emit_runtime_event == nullptr) {
        return;
    }
    const auto input = input_type.toUtf8();
    OrnaRuntimeEventV1 event{};
    event.kind = ORNA_RUNTIME_EVENT_ACTION;
    event.as.action.surface = surface;
    event.as.action.node = node;
    event.as.action.action = action;
    event.as.action.payload = OrnaValueRefV1{
        0,
        OrnaStringView{input.constData(), static_cast<std::size_t>(input.size())},
        OrnaBytesView{nullptr, 0},
    };
    runtime->in_callback = true;
    try {
        (void)runtime->client.emit_runtime_event(runtime->client.context, runtime_handle(runtime), &event);
    } catch (...) {
        // A foreign callback must not unwind through the C ABI.
    }
    runtime->in_callback = false;
}

void connect_action(Runtime *runtime,
                     Surface &surface,
                     Node &node,
                     const std::string &event_name,
                     const ProjectedAction &binding) {
    auto *button = qobject_cast<QPushButton *>(node.widget.data());
    if (button == nullptr) {
        throw std::runtime_error("action target does not provide a Qt signal");
    }
    const auto connection = QObject::connect(button, &QPushButton::clicked, [runtime, surface_id = surface.handle,
                                                                              node_id = node.handle,
                                                                              action_id = binding.handle,
                                                                              input_type = binding.input_type]() {
        emit_action(runtime, surface_id, node_id, action_id, input_type);
    });
    if (!connection) {
        throw std::runtime_error("Qt action connection failed");
    }
    node.actions[event_name] = ActionBinding{binding.handle, binding.input_type, connection};
}

void destroy_node(Runtime *runtime,
                  Surface &surface,
                  OrnaNodeHandle handle,
                  bool unregister_handles = true) {
    try {
        if (surface.nodes.find(handle) == surface.nodes.end()) {
            return;
        }
        std::vector<OrnaNodeHandle> pending{handle};
        std::vector<OrnaNodeHandle> subtree;
        subtree.reserve(surface.nodes.size());
        while (!pending.empty()) {
            const auto current = pending.back();
            pending.pop_back();
            const auto node = surface.nodes.find(current);
            if (node == surface.nodes.end()) {
                continue;
            }
            subtree.push_back(current);
            for (const auto child : node->second.children) {
                pending.push_back(child);
            }
        }
        for (auto iterator = subtree.rbegin(); iterator != subtree.rend(); ++iterator) {
            const auto found = surface.nodes.find(*iterator);
            if (found == surface.nodes.end()) {
                continue;
            }
            for (const auto &[event, binding] : found->second.actions) {
                (void)event;
                QObject::disconnect(binding.connection);
                if (unregister_handles && runtime != nullptr) {
                    runtime->live_action_handles.erase(binding.handle);
                    runtime->retired_action_handles.insert(binding.handle);
                }
            }
            detach_widget(surface, found->second);
            if (found->second.widget != nullptr && found->second.widget != surface.window) {
                delete found->second.widget;
            }
            if (found->second.parent != 0) {
                const auto parent = surface.nodes.find(found->second.parent);
                if (parent != surface.nodes.end()) {
                    remove_child_reference(parent->second.children, *iterator);
                }
            }
            if (unregister_handles && runtime != nullptr) {
                runtime->live_node_handles.erase(*iterator);
                runtime->retired_node_handles.insert(*iterator);
            }
            surface.nodes.erase(found);
        }
    } catch (...) {
        // Destruction must not unwind through the C ABI.
    }
}
bool emit_surface_closed(Runtime *runtime, OrnaSurfaceHandle surface) {
    if (runtime == nullptr || runtime->client.emit_runtime_event == nullptr || runtime->terminal) {
        return true;
    }
    OrnaRuntimeEventV1 event{};
    event.kind = ORNA_RUNTIME_EVENT_SURFACE_CLOSED;
    event.as.surface_closed.surface = surface;
    OrnaStatus callback_status = internal_error();
    runtime->in_callback = true;
    try {
        callback_status =
            runtime->client.emit_runtime_event(runtime->client.context, runtime_handle(runtime), &event);
    } catch (...) {
        // A foreign callback must not unwind through the C ABI.
    }
    runtime->in_callback = false;
    return callback_status.code == ORNA_STATUS_OK;
}

void RuntimeWindow::closeEvent(QCloseEvent *event) {
    if (!suppress_close_event_ && runtime_ != nullptr && !runtime_->terminal) {
        const auto found = runtime_->surfaces.find(surface_);
        if (found != runtime_->surfaces.end() && !found->second.native_closed) {
            found->second.native_closed = true;
            found->second.visible = false;
            found->second.close_event_delivered = emit_surface_closed(runtime_, surface_);
        }
    }
    event->accept();
}

void destroy_surface_widgets(Runtime *runtime,
                             Surface &surface,
                             bool emit_event = true,
                             bool unregister_handles = true) {
    if (emit_event && !surface.close_event_delivered) {
        surface.native_closed = true;
        surface.close_event_delivered = emit_surface_closed(runtime, surface.handle);
    }
    if (runtime == nullptr && surface.window != nullptr) {
        static_cast<RuntimeWindow *>(surface.window.data())->suppress_close_event();
    }
    if (surface.window != nullptr) {
        surface.window->close();
    }
    while (!surface.nodes.empty()) {
        destroy_node(runtime, surface, surface.nodes.begin()->first, unregister_handles);
    }
    if (surface.window != nullptr) {
        delete surface.window;
        surface.window = nullptr;
    }
}

void reap_closed_surfaces(Runtime *runtime) {
    if (runtime == nullptr) {
        return;
    }
    for (auto iterator = runtime->surfaces.begin(); iterator != runtime->surfaces.end();) {
        auto &surface = iterator->second;
        if (!surface.native_closed) {
            ++iterator;
            continue;
        }
        if (!surface.close_event_delivered) {
            surface.close_event_delivered = emit_surface_closed(runtime, surface.handle);
            if (!surface.close_event_delivered) {
                ++iterator;
                continue;
            }
        }
        auto current = iterator++;
        destroy_surface_widgets(runtime, current->second, false);
        runtime->surfaces.erase(current);
    }
}

void materialise_surface(Runtime *runtime,
                          const Surface &current,
                          const std::unordered_map<OrnaNodeHandle, ProjectedNode> &projected,
                          std::uint64_t semantic_revision,
                          Surface &staged) {
    staged.handle = current.handle;
    staged.base_title = current.base_title;
    staged.semantic_revision = semantic_revision;
    staged.visible = current.visible;
    staged.window = new RuntimeWindow(runtime, current.handle);
    if (staged.window == nullptr) {
        throw std::bad_alloc();
    }
    staged.window->setWindowTitle(current.base_title);
    ensure_layout(staged.window, QStringLiteral("std.ui.column"));
    for (const auto &[handle, projected_node] : projected) {
        Node node;
        node.handle = handle;
        node.parent = projected_node.parent;
        node.contract = projected_node.contract;
        node.slot = projected_node.slot;
        node.children = projected_node.children;
        node.properties = projected_node.properties;
        node.property_types = projected_node.property_types;
        node.has_explicit_key = projected_node.has_explicit_key;
        node.explicit_key_type = projected_node.explicit_key_type;
        node.explicit_key_bytes = projected_node.explicit_key_bytes;
        node.widget = create_widget(staged, node.contract);
        if (node.widget == nullptr) {
            throw std::bad_alloc();
        }
        staged.nodes.emplace(handle, std::move(node));
    }
    for (const auto &[handle, projected_node] : projected) {
        auto found = staged.nodes.find(handle);
        if (found == staged.nodes.end()) {
            throw std::runtime_error("materialised node is missing");
        }
        for (const auto &[name, value] : projected_node.properties) {
            const auto type = projected_node.property_types.find(name);
            if (type == projected_node.property_types.end()) {
                throw std::runtime_error("materialised property type is missing");
            }
            apply_property(found->second, name, type->second, value);
        }
        for (const auto &[event, action] : projected_node.actions) {
            connect_action(runtime, staged, found->second, event, action);
        }
    }
    for (const auto &[handle, projected_node] : projected) {
        if (staged.nodes.find(handle) == staged.nodes.end()) {
            throw std::runtime_error("materialised parent is missing");
        }
        for (const auto child_handle : projected_node.children) {
            auto child = staged.nodes.find(child_handle);
            if (child == staged.nodes.end()) {
                throw std::runtime_error("materialised child is missing");
            }
            attach_widget(staged, child->second);
        }
    }
    staged.window->setVisible(staged.visible);
}


void append_json_string(QByteArray &output, const QString &value) {
    output.append('"');
    const auto bytes = value.toUtf8();
    for (const auto byte : bytes) {
        switch (static_cast<unsigned char>(byte)) {
        case '"':
            output.append("\\\"");
            break;
        case '\\':
            output.append("\\\\");
            break;
        case '\b':
            output.append("\\b");
            break;
        case '\f':
            output.append("\\f");
            break;
        case '\n':
            output.append("\\n");
            break;
        case '\r':
            output.append("\\r");
            break;
        case '\t':
            output.append("\\t");
            break;
        default:
            if (static_cast<unsigned char>(byte) < 0x20) {
                output.append("\\u00");
                output.append(QByteArray::number(static_cast<unsigned char>(byte), 16).rightJustified(2, '0'));
            } else {
                output.append(byte);
            }
            break;
        }
    }
    output.append('"');
}

void append_hex(QByteArray &output, const QByteArray &bytes) {
    static constexpr char digits[] = "0123456789abcdef";
    for (const auto byte : bytes) {
        const auto value = static_cast<unsigned char>(byte);
        output.append(digits[value >> 4]);
        output.append(digits[value & 0x0f]);
    }
}

void append_value(QByteArray &output, const QString &type, const QByteArray &bytes) {
    output.append("{\"type\":");
    append_json_string(output, type);
    output.append(",\"value\":\"");
    append_hex(output, bytes);
    output.append("\"}");
}

void append_node_json(QByteArray &output,
                      const Surface &surface,
                      OrnaNodeHandle handle,
                      std::size_t depth) {
    if (depth > kMaxDepth) {
        throw std::runtime_error("UI tree depth exceeds the Qt v1 limit");
    }
    const auto node = surface.nodes.find(handle);
    if (node == surface.nodes.end()) {
        throw std::runtime_error("UI tree references a missing node");
    }
    const auto &value = node->second;
    output.append("{\"actions\":{");
    for (auto iterator = value.actions.begin(); iterator != value.actions.end(); ++iterator) {
        if (iterator != value.actions.begin()) {
            output.append(',');
        }
        append_json_string(output, QString::fromUtf8(iterator->first.data(),
                                                      static_cast<qsizetype>(iterator->first.size())));
        output.append(":{\"action_id\":");
        append_json_string(output, QString::number(static_cast<qint64>(iterator->second.handle)));
        output.append(",\"debug_kind\":null,\"input_type\":");
        append_json_string(output, iterator->second.input_type);
        output.append('}');
    }
    output.append("},\"call_site_id\":null,\"contract\":{\"id\":");
    append_json_string(output, value.contract);
    output.append(",\"name\":");
    append_json_string(output, value.contract);
    output.append(",\"version\":\"1.0\"},\"function_instance_id\":null,\"key\":");
    if (value.has_explicit_key) {
        append_value(output, value.explicit_key_type, value.explicit_key_bytes);
    } else {
        append_value(output, QString(), QByteArray());
    }
    output.append(",\"kind\":\"node\",\"properties\":{");
    for (auto iterator = value.properties.begin(); iterator != value.properties.end(); ++iterator) {
        if (iterator != value.properties.begin()) {
            output.append(',');
        }
        append_json_string(output, QString::fromUtf8(iterator->first.data(),
                                                      static_cast<qsizetype>(iterator->first.size())));
        output.append(':');
        const auto type = value.property_types.find(iterator->first);
        if (type == value.property_types.end()) {
            throw std::runtime_error("UI property type is missing");
        }
        append_value(output, type->second, iterator->second);
    }
    output.append("},\"slots\":{");
    std::map<QString, std::vector<OrnaNodeHandle>> slots_by_name;
    for (const auto child_handle : value.children) {
        const auto child = surface.nodes.find(child_handle);
        if (child == surface.nodes.end()) {
            throw std::runtime_error("UI tree has a missing child");
        }
        slots_by_name[child->second.slot].push_back(child_handle);
    }
    for (auto iterator = slots_by_name.begin(); iterator != slots_by_name.end(); ++iterator) {
        if (iterator != slots_by_name.begin()) {
            output.append(',');
        }
        append_json_string(output, iterator->first);
        output.append(":[");
        for (std::size_t index = 0; index < iterator->second.size(); ++index) {
            if (index != 0) {
                output.append(',');
            }
            append_node_json(output, surface, iterator->second[index], depth + 1);
        }
        output.append(']');
    }
    output.append("}}");
}

QByteArray semantic_state(const Surface &surface) {
    std::vector<OrnaNodeHandle> roots;
    for (const auto &[handle, node] : surface.nodes) {
        if (node.parent == 0) {
            roots.push_back(handle);
        }
    }
    QByteArray body;
    if (roots.empty()) {
        body = "{\"kind\":\"empty\"}";
    } else if (roots.size() == 1) {
        append_node_json(body, surface, roots.front(), 0);
    } else {
        body.append("{\"children\":[");
        for (std::size_t index = 0; index < roots.size(); ++index) {
            if (index != 0) {
                body.append(',');
            }
            append_node_json(body, surface, roots[index], 0);
        }
        body.append("],\"kind\":\"fragment\"}");
    }
    if (body.size() > static_cast<qsizetype>(std::numeric_limits<std::uint32_t>::max())
        || body.size() + 14 > static_cast<qsizetype>(kMaxBytes)) {
        throw std::runtime_error("UI semantic state is too large");
    }
    QByteArray frame("ORNA-UI/1 ");
    const auto length = static_cast<std::uint32_t>(body.size());
    frame.append(static_cast<char>((length >> 24) & 0xff));
    frame.append(static_cast<char>((length >> 16) & 0xff));
    frame.append(static_cast<char>((length >> 8) & 0xff));
    frame.append(static_cast<char>(length & 0xff));
    frame.append(body);
    return frame;
}

const OrnaRuntimeDescriptorV1 *descriptor() {
    static OrnaContractVersionV1 contracts[sizeof(kContracts) / sizeof(kContracts[0])];
    static OrnaSinkOfferV1 sinks[1];
    static OrnaRuntimeDescriptorV1 value{};
    static bool initialised = false;
    if (!initialised) {
        for (std::size_t index = 0; index < sizeof(kContracts) / sizeof(kContracts[0]); ++index) {
            contracts[index] = OrnaContractVersionV1{
                string_view(kContracts[index]),
                1,
                0,
                nullptr,
                0,
            };
        }
        sinks[0] = OrnaSinkOfferV1{string_view(kUiType), nullptr, 0, 0, 0};
        value = OrnaRuntimeDescriptorV1{
            ORNA_RUNTIME_ABI_V1_MAJOR,
            ORNA_RUNTIME_ABI_V1_MINOR,
            string_view(kRuntimeName),
            string_view(kRuntimeVersion),
            string_view(kBuildId),
            string_view(kPlatform),
            ORNA_THREAD_MODEL_CALLER_PUMPS,
            ORNA_RUNTIME_FEATURE_MULTIPLE_WINDOWS,
            sinks,
            1,
            contracts,
            sizeof(kContracts) / sizeof(kContracts[0]),
        };
        initialised = true;
    }
    return &value;
}

OrnaStatus create_runtime(const OrnaRuntimeCreateOptionsV1 *options, OrnaRuntimeHandle *output) {
    if (output != nullptr) {
        *output = 0;
    }
    try {
        if (options == nullptr || output == nullptr || options->client == nullptr
            || options->client->abi_major != ORNA_RUNTIME_ABI_V1_MAJOR
            || options->client->abi_minor > ORNA_RUNTIME_ABI_V1_MINOR
            || !valid_string_view(options->locale) || !valid_string_view(options->timezone)
            || !valid_string_view(options->theme) || !valid_string_view(options->accessibility_preferences_json)
            || !valid_string_view(options->runtime_configuration_json, kMaxBytes)) {
            return invalid();
        }
        if (g_application != nullptr && g_application_thread != std::this_thread::get_id()) {
            return result(ORNA_STATUS_BUSY, "Qt application belongs to another thread");
        }
        auto runtime = std::make_unique<Runtime>();
        runtime->client = *options->client;
        runtime->handle = next_handle();
        if (runtime->handle == 0) {
            return internal_error();
        }
        runtime->owner_thread = std::this_thread::get_id();
        bool created_application = false;
        if (g_application == nullptr) {
            if (auto *existing = qobject_cast<QApplication *>(QCoreApplication::instance())) {
                if (existing->thread() != QThread::currentThread()) {
                    return result(ORNA_STATUS_BUSY, "Qt application belongs to another thread");
                }
                g_application = existing;
                g_application_owned = false;
                g_saved_quit_on_last_window_closed = existing->quitOnLastWindowClosed();
                existing->setQuitOnLastWindowClosed(false);
                g_restore_quit_on_last_window_closed = true;
                g_application_thread = runtime->owner_thread;
            } else if (QCoreApplication::instance() != nullptr) {
                return unsupported();
            } else {
                static int argc = 1;
                static char application_name[] = "orna-runtime-qt";
                static char *argv[] = {application_name, nullptr};
                g_application = new QApplication(argc, argv);
                g_application_owned = true;
                g_application_thread = runtime->owner_thread;
                g_application->setQuitOnLastWindowClosed(false);
                created_application = true;
            }
        }
        try {
            const auto inserted = g_live_runtimes.emplace(runtime->handle, runtime.get());
            if (!inserted.second) {
                release_application_after_failed_create(created_application);
                return internal_error();
            }
        } catch (...) {
            release_application_after_failed_create(created_application);
            return internal_error();
        }
        ++g_runtime_count;
        *output = runtime_handle(runtime.get());
        runtime.release();
        return ok();
    } catch (...) {
        return internal_error();
    }
}

} // namespace

extern "C" {

ORNA_RUNTIME_EXPORT const OrnaRuntimeApiV1 *orna_runtime_query_v1(void) {
    static const OrnaRuntimeApiV1 api{
        ORNA_RUNTIME_ABI_V1_MAJOR,
        ORNA_RUNTIME_ABI_V1_MINOR,
        descriptor,
        create_runtime,
        [](OrnaRuntimeHandle handle) {
            auto *runtime = runtime_from_handle(handle);
            if (runtime == nullptr || !on_owner_thread(runtime) || runtime->in_callback) {
                return;
            }
            if (!runtime->terminal) {
                return;
            }
            for (auto &[surface_id, surface] : runtime->surfaces) {
                (void)surface_id;
                destroy_surface_widgets(runtime, surface);
            }
            runtime->surfaces.clear();
            runtime->terminal = true;
            g_live_runtimes.erase(runtime->handle);
            if (g_runtime_count > 0) {
                --g_runtime_count;
            }
            if (g_runtime_count == 0) {
                if (g_application_owned) {
                    delete g_application;
                    g_application = nullptr;
                    g_application_owned = false;
                    g_application_thread = {};
                } else if (g_restore_quit_on_last_window_closed && g_application != nullptr) {
                    g_application->setQuitOnLastWindowClosed(g_saved_quit_on_last_window_closed);
                    g_restore_quit_on_last_window_closed = false;
                    g_application = nullptr;
                    g_application_thread = {};
                }
            }
            delete runtime;
        },
        [](OrnaRuntimeHandle handle) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            return unsupported();
        },
        [](OrnaRuntimeHandle handle, std::uint32_t timeout_ms) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            if (timeout_ms > 60000) {
                return invalid();
            }
            try {
                QCoreApplication::processEvents(QEventLoop::AllEvents, static_cast<int>(timeout_ms));
                reap_closed_surfaces(runtime);
                return ok();
            } catch (...) {
                return internal_error();
            }
        },
        [](OrnaRuntimeHandle handle) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = draining_or_terminal(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            if (runtime->terminal) {
                return ok();
            }
            runtime->draining = true;
            while (!runtime->surfaces.empty()) {
                auto found = runtime->surfaces.begin();
                destroy_surface_widgets(runtime, found->second);
                if (!found->second.close_event_delivered) {
                    return internal_error();
                }
                runtime->surfaces.erase(found);
            }
            runtime->terminal = true;
            return ok();
        },
        [](OrnaRuntimeHandle handle, const OrnaSurfaceCreateOptionsV1 *options, OrnaSurfaceHandle *output) {
            if (output != nullptr) {
                *output = 0;
            }
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            try {
                if (options == nullptr || output == nullptr || !valid_string_view(options->surface_kind)
                    || !valid_string_view(options->title) || !valid_string_view(options->state_profile)
                    || !valid_bytes_view(options->opaque_runtime_restore_state)) {
                    return invalid();
                }
                if (options->opaque_runtime_restore_state.len != 0) {
                    return unsupported();
                }
                const auto kind = read_string(options->surface_kind);
                if (kind != QLatin1String("window")) {
                    return unsupported();
                }
                Surface surface;
                surface.handle = next_handle();
                if (surface.handle == 0) {
                    return internal_error();
                }
                auto window = std::make_unique<RuntimeWindow>(runtime, surface.handle);
                surface.window = window.get();
                surface.base_title = read_string(options->title);
                surface.window->setWindowTitle(read_string(options->title));
                ensure_layout(surface.window, QStringLiteral("std.ui.column"));
                const auto inserted = runtime->surfaces.emplace(surface.handle, std::move(surface));
                if (!inserted.second) {
                    return internal_error();
                }
                window.release();
                *output = inserted.first->first;
                return ok();
            } catch (...) {
                return internal_error();
            }
        },
        [](OrnaRuntimeHandle handle, OrnaSurfaceHandle surface_handle) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            const auto found = runtime->surfaces.find(surface_handle);
            if (found == runtime->surfaces.end()) {
                return not_found();
            }
            if (found->second.native_closed && found->second.close_event_delivered) {
                return not_found();
            }
            destroy_surface_widgets(runtime, found->second);
            if (!found->second.close_event_delivered) {
                return internal_error();
            }
            runtime->surfaces.erase(found);
            return ok();
        },
        [](OrnaRuntimeHandle handle, OrnaSurfaceHandle surface_handle, const OrnaUiBatchV1 *batch) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            if (batch == nullptr || batch->operation_count > kMaxOperations
                || (batch->operation_count > 0 && batch->operations == nullptr)) {
                return invalid();
            }
            const auto found = runtime->surfaces.find(surface_handle);
            if (found == runtime->surfaces.end() || found->second.native_closed) {
                return not_found();
            }
            if (batch->semantic_revision <= found->second.semantic_revision) {
                return result(ORNA_STATUS_STALE_REVISION, kStale);
            }
            try {
                for (std::size_t index = 0; index < batch->operation_count; ++index) {
                    const auto &operation = batch->operations[index];
                    if (operation.kind == ORNA_UI_OP_SET_FOCUS || operation.kind == ORNA_UI_OP_SET_ACCESSIBILITY) {
                        return unsupported();
                    }
                    if (operation.kind == ORNA_UI_OP_UNMOUNT_NODE
                        && runtime->retired_node_handles.find(operation.as.unmount_node)
                               != runtime->retired_node_handles.end()) {
                        return not_found();
                    }
                    if (operation.kind == ORNA_UI_OP_UNBIND_ACTION
                        && runtime->retired_action_handles.find(operation.as.bind_action.action)
                               != runtime->retired_action_handles.end()) {
                        return not_found();
                    }
                    if (operation.kind == ORNA_UI_OP_MOUNT_NODE) {
                        const auto &mount = operation.as.mount_node;
                        if (!valid_string_view(mount.contract_name) || mount.contract_name.len == 0) {
                            return invalid();
                        }
                        const auto contract = operation_contract(mount.contract_name);
                        if (!is_allowed_contract(contract)) {
                            return unsupported();
                        }
                        if (mount.contract_major != 1 || mount.contract_minor != 0) {
                            return unsupported();
                        }
                    }
                }
            } catch (...) {
                return internal_error();
            }
            std::unordered_map<OrnaNodeHandle, ProjectedNode> projected;
            std::unordered_set<OrnaNodeHandle> staged_live_nodes;
            std::unordered_set<OrnaNodeHandle> staged_retired_nodes;
            std::unordered_set<OrnaActionHandle> staged_live_actions;
            std::unordered_set<OrnaActionHandle> staged_retired_actions;
            Surface staged;
            try {
                if (!validate_batch(*runtime, found->second, *batch, projected)) {
                    return invalid();
                }
                staged_live_nodes = runtime->live_node_handles;
                staged_retired_nodes = runtime->retired_node_handles;
                staged_live_actions = runtime->live_action_handles;
                staged_retired_actions = runtime->retired_action_handles;
                materialise_surface(runtime, found->second, projected, batch->semantic_revision, staged);
                for (const auto &[node_handle, node] : projected) {
                    (void)node;
                    if (found->second.nodes.find(node_handle) == found->second.nodes.end()) {
                        if (staged_live_nodes.find(node_handle) != staged_live_nodes.end()
                            || staged_retired_nodes.find(node_handle) != staged_retired_nodes.end()) {
                            throw std::runtime_error("node handle registry collision");
                        }
                        staged_live_nodes.insert(node_handle);
                    }
                }
                for (const auto &[node_handle, node] : projected) {
                    const auto current_node = found->second.nodes.find(node_handle);
                    for (const auto &[event, action] : node.actions) {
                        (void)event;
                        bool current_action = false;
                        if (current_node != found->second.nodes.end()) {
                            for (const auto &[current_event, current_binding] : current_node->second.actions) {
                                (void)current_event;
                                if (current_binding.handle == action.handle) {
                                    current_action = true;
                                    break;
                                }
                            }
                        }
                        if (!current_action) {
                            if (staged_live_actions.find(action.handle) != staged_live_actions.end()
                                || staged_retired_actions.find(action.handle) != staged_retired_actions.end()) {
                                throw std::runtime_error("action handle registry collision");
                            }
                            staged_live_actions.insert(action.handle);
                        }
                    }
                }
                for (const auto &[node_handle, node] : found->second.nodes) {
                    if (projected.find(node_handle) == projected.end()) {
                        staged_live_nodes.erase(node_handle);
                        staged_retired_nodes.insert(node_handle);
                    }
                    for (const auto &[event, binding] : node.actions) {
                        (void)event;
                        bool retained = false;
                        for (const auto &[projected_handle, projected_node] : projected) {
                            (void)projected_handle;
                            for (const auto &[projected_event, projected_action] : projected_node.actions) {
                                (void)projected_event;
                                if (projected_action.handle == binding.handle) {
                                    retained = true;
                                    break;
                                }
                            }
                            if (retained) {
                                break;
                            }
                        }
                        if (!retained) {
                            staged_live_actions.erase(binding.handle);
                            staged_retired_actions.insert(binding.handle);
                        }
                    }
                }
                for (std::size_t index = 0; index < batch->operation_count; ++index) {
                    const auto &operation = batch->operations[index];
                    if (operation.kind == ORNA_UI_OP_MOUNT_NODE
                        && projected.find(operation.as.mount_node.node) == projected.end()) {
                        staged_retired_nodes.insert(operation.as.mount_node.node);
                    }
                    if (operation.kind == ORNA_UI_OP_BIND_ACTION
                        && !projected_action_exists(projected, operation.as.bind_action.action)) {
                        staged_retired_actions.insert(operation.as.bind_action.action);
                    }
                }
                runtime->live_node_handles.swap(staged_live_nodes);
                runtime->retired_node_handles.swap(staged_retired_nodes);
                runtime->live_action_handles.swap(staged_live_actions);
                runtime->retired_action_handles.swap(staged_retired_actions);
                Surface old = std::move(found->second);
                found->second = std::move(staged);
                destroy_surface_widgets(nullptr, old, false, false);
                return ok();
            } catch (...) {
                destroy_surface_widgets(nullptr, staged, false, false);
                return internal_error();
            }
        },
        [](OrnaRuntimeHandle handle, OrnaSurfaceHandle surface_handle, std::uint8_t visible) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            if (visible > 1) {
                return invalid();
            }
            const auto found = runtime->surfaces.find(surface_handle);
            if (found == runtime->surfaces.end() || found->second.native_closed) {
                return not_found();
            }
            found->second.visible = visible != 0;
            if (found->second.window != nullptr) {
                found->second.window->setVisible(found->second.visible);
            }
            return ok();
        },
        [](OrnaRuntimeHandle handle, OrnaSurfaceHandle surface_handle, OrnaOwnedBytes *output) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            const auto found = runtime->surfaces.find(surface_handle);
            if (found == runtime->surfaces.end() || found->second.native_closed) {
                return not_found();
            }
            try {
                return owned_bytes(semantic_state(found->second), output);
            } catch (...) {
                return internal_error();
            }
        },
        [](OrnaRuntimeHandle handle, OrnaSurfaceHandle surface_handle, OrnaOwnedBytes *output) {
            (void)output;
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            const auto found = runtime->surfaces.find(surface_handle);
            if (found == runtime->surfaces.end() || found->second.native_closed) {
                return not_found();
            }
            return unsupported();
        },
        [](OrnaRuntimeHandle handle, OrnaRequestHandle, OrnaValueRefV1) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            return unsupported();
        },
        [](OrnaRuntimeHandle handle, OrnaRequestHandle) {
            auto *runtime = runtime_from_handle(handle);
            const auto status = operational(runtime);
            if (status.code != ORNA_STATUS_OK) {
                return status;
            }
            return unsupported();
        },
    };
    return &api;
}

} // extern "C"
