#include <orna_runtime_abi_v1.h>

#include <QApplication>
#include <QColor>
#include <QImage>

#include <QLabel>
#include <QLineEdit>
#include <QPixmap>
#include <QPushButton>
#include <QString>
#include <QWidget>

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace {

struct VisualAssertionFailure {
    const char *condition;
    int line;
};

[[noreturn]] void fail_visual(const char *condition, int line) {
    throw VisualAssertionFailure{condition, line};
}

#define REQUIRE_VISUAL(condition) \
    do { \
        if (!(condition)) { \
            fail_visual(#condition, __LINE__); \
        } \
    } while (false)

OrnaStringView view(const char *value) {
    return OrnaStringView{value, std::strlen(value)};
}

OrnaStatus emit_event(void *, OrnaRuntimeHandle, const OrnaRuntimeEventV1 *) {
    return OrnaStatus{ORNA_STATUS_OK, OrnaStringView{nullptr, 0}};
}

OrnaStatus fail_model(void *, OrnaRequestHandle, OrnaStatus failure) {
    return failure;
}

struct RuntimeGuard {
    const OrnaRuntimeApiV1 *api = nullptr;
    OrnaRuntimeHandle runtime = 0;

    ~RuntimeGuard() {
        if (api == nullptr || runtime == 0) {
            return;
        }
        (void)api->request_shutdown(runtime);
        api->destroy(runtime);
    }
};

} // namespace

int main(int argc, char **argv) {
    try {
        REQUIRE_VISUAL(argc == 2);
        const QString output_path = QString::fromLocal8Bit(argv[1]);
        REQUIRE_VISUAL(!output_path.isEmpty());

        const auto *api = orna_runtime_query_v1();
        REQUIRE_VISUAL(api != nullptr);
        REQUIRE_VISUAL(api->create != nullptr);
        REQUIRE_VISUAL(api->destroy != nullptr);
        REQUIRE_VISUAL(api->create_surface != nullptr);
        REQUIRE_VISUAL(api->apply_ui_batch != nullptr);
        REQUIRE_VISUAL(api->set_surface_visible != nullptr);
        REQUIRE_VISUAL(api->poll_event_loop != nullptr);
        REQUIRE_VISUAL(api->request_shutdown != nullptr);

        OrnaClientApiV1 client{
            1,
            0,
            nullptr,
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

        RuntimeGuard runtime_guard{api};
        OrnaRuntimeHandle runtime = 0;
        const auto create_status = api->create(&create_options, &runtime);
        REQUIRE_VISUAL(create_status.code == ORNA_STATUS_OK);
        REQUIRE_VISUAL(runtime != 0);
        runtime_guard.runtime = runtime;

        const char window_title[] = "OrnaDB Qt visual smoke";
        OrnaSurfaceCreateOptionsV1 surface_options{
            view("window"),
            view(window_title),
            view("local"),
            OrnaBytesView{nullptr, 0},
        };
        OrnaSurfaceHandle surface = 0;
        const auto surface_status = api->create_surface(runtime, &surface_options, &surface);
        REQUIRE_VISUAL(surface_status.code == ORNA_STATUS_OK);
        REQUIRE_VISUAL(surface != 0);

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
        OrnaMountNodeV1 label_mount{
            101,
            100,
            view("content"),
            0,
            view("std.ui.text"),
            1,
            0,
            no_key,
        };
        OrnaMountNodeV1 input_mount{
            102,
            100,
            view("content"),
            1,
            view("std.ui.text_input"),
            1,
            0,
            no_key,
        };
        OrnaMountNodeV1 button_mount{
            103,
            100,
            view("content"),
            2,
            view("std.ui.button"),
            1,
            0,
            no_key,
        };

        const char label_text[] = "Rendered Qt label";
        const char input_placeholder[] = "Type a name";
        const char button_label[] = "Continue";
        OrnaSetPropertyV1 root_title_property{
            100,
            view("title"),
            OrnaValueRefV1{
                0,
                view("std.text"),
                OrnaBytesView{reinterpret_cast<const std::uint8_t *>(window_title), sizeof(window_title) - 1},
            },
        };
        OrnaSetPropertyV1 label_text_property{
            101,
            view("text"),
            OrnaValueRefV1{
                0,
                view("std.text"),
                OrnaBytesView{reinterpret_cast<const std::uint8_t *>(label_text), sizeof(label_text) - 1},
            },
        };
        OrnaSetPropertyV1 input_placeholder_property{
            102,
            view("placeholder"),
            OrnaValueRefV1{
                0,
                view("std.text"),
                OrnaBytesView{reinterpret_cast<const std::uint8_t *>(input_placeholder),
                              sizeof(input_placeholder) - 1},
            },
        };
        OrnaSetPropertyV1 button_label_property{
            103,
            view("label"),
            OrnaValueRefV1{
                0,
                view("std.text"),
                OrnaBytesView{reinterpret_cast<const std::uint8_t *>(button_label), sizeof(button_label) - 1},
            },
        };

        OrnaUiOperationV1 operations[8]{};
        operations[0].kind = ORNA_UI_OP_MOUNT_NODE;
        operations[0].as.mount_node = root_mount;
        operations[1].kind = ORNA_UI_OP_SET_PROPERTY;
        operations[1].as.set_property = root_title_property;
        operations[2].kind = ORNA_UI_OP_MOUNT_NODE;
        operations[2].as.mount_node = label_mount;
        operations[3].kind = ORNA_UI_OP_SET_PROPERTY;
        operations[3].as.set_property = label_text_property;
        operations[4].kind = ORNA_UI_OP_MOUNT_NODE;
        operations[4].as.mount_node = input_mount;
        operations[5].kind = ORNA_UI_OP_SET_PROPERTY;
        operations[5].as.set_property = input_placeholder_property;
        operations[6].kind = ORNA_UI_OP_MOUNT_NODE;
        operations[6].as.mount_node = button_mount;
        operations[7].kind = ORNA_UI_OP_SET_PROPERTY;
        operations[7].as.set_property = button_label_property;
        OrnaUiBatchV1 batch{1, operations, sizeof(operations) / sizeof(operations[0])};
        const auto batch_status = api->apply_ui_batch(runtime, surface, &batch);
        REQUIRE_VISUAL(batch_status.code == ORNA_STATUS_OK);

        const auto visible_status = api->set_surface_visible(runtime, surface, 1);
        REQUIRE_VISUAL(visible_status.code == ORNA_STATUS_OK);
        const auto poll_status = api->poll_event_loop(runtime, 50);
        REQUIRE_VISUAL(poll_status.code == ORNA_STATUS_OK);

        QWidget *window = nullptr;
        for (QWidget *candidate : QApplication::topLevelWidgets()) {
            if (candidate != nullptr && candidate->windowTitle() == QString::fromUtf8(window_title)) {
                window = candidate;
                break;
            }
        }
        REQUIRE_VISUAL(window != nullptr);
        REQUIRE_VISUAL(window->isVisible());
        REQUIRE_VISUAL(window->width() > 0 && window->height() > 0);

        auto *label = window->findChild<QLabel *>();
        auto *input = window->findChild<QLineEdit *>();
        auto *button = window->findChild<QPushButton *>();
        REQUIRE_VISUAL(label != nullptr);
        REQUIRE_VISUAL(input != nullptr);
        REQUIRE_VISUAL(button != nullptr);
        REQUIRE_VISUAL(label->text() == QString::fromUtf8(label_text));
        REQUIRE_VISUAL(input->placeholderText() == QString::fromUtf8(input_placeholder));
        REQUIRE_VISUAL(button->text() == QString::fromUtf8(button_label));

        REQUIRE_VISUAL(label->isVisible());
        REQUIRE_VISUAL(label->width() > 0 && label->height() > 0);
        REQUIRE_VISUAL(window->rect().intersects(label->geometry()));
        REQUIRE_VISUAL(input->isVisible());
        REQUIRE_VISUAL(input->width() > 0 && input->height() > 0);
        REQUIRE_VISUAL(window->rect().intersects(input->geometry()));
        REQUIRE_VISUAL(button->isVisible());
        REQUIRE_VISUAL(button->width() > 0 && button->height() > 0);
        REQUIRE_VISUAL(window->rect().intersects(button->geometry()));

        const QPixmap capture = window->grab();
        REQUIRE_VISUAL(!capture.isNull());
        REQUIRE_VISUAL(capture.width() > 0 && capture.height() > 0);
        const QImage image = capture.toImage().convertToFormat(QImage::Format_RGB32);
        const QColor background = image.pixelColor(0, 0);
        std::size_t rendered_pixels = 0;
        for (int y = 0; y < image.height(); ++y) {
            for (int x = 0; x < image.width(); ++x) {
                const auto color = image.pixelColor(x, y);
                const auto difference = std::abs(color.red() - background.red())
                    + std::abs(color.green() - background.green())
                    + std::abs(color.blue() - background.blue());
                if (difference > 24) {
                    ++rendered_pixels;
                }
            }
        }
        REQUIRE_VISUAL(rendered_pixels > 100);
        REQUIRE_VISUAL(capture.save(output_path, "PNG"));

        const auto shutdown_status = api->request_shutdown(runtime);
        REQUIRE_VISUAL(shutdown_status.code == ORNA_STATUS_OK);
        api->destroy(runtime);
        runtime_guard.runtime = 0;
        return EXIT_SUCCESS;
    } catch (const VisualAssertionFailure &failure) {
        std::fprintf(stderr,
                     "Qt runtime visual condition failed at line %d: %s\n",
                     failure.line,
                     failure.condition);
        return EXIT_FAILURE;
    }
}
