/* Test input for a private headless compositor, never the user's desktop.
 * Build with the client header/code generated from wlr-virtual-pointer XML. */
#include <linux/input-event-codes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "virtual-pointer.h"

static struct zwlr_virtual_pointer_manager_v1 *manager;
static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data;
    if (!strcmp(interface, "zwlr_virtual_pointer_manager_v1"))
        manager = wl_registry_bind(registry, name,
            &zwlr_virtual_pointer_manager_v1_interface, version < 2 ? version : 2);
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener listener = {global, removed};
int main(void) {
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 1;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &listener, NULL);
    if (wl_display_roundtrip(display) < 0 || !manager) return 2;
    struct zwlr_virtual_pointer_v1 *pointer =
        zwlr_virtual_pointer_manager_v1_create_virtual_pointer(manager, NULL);
    /* Announce the pointer capability before clients bind their seats. Do not
     * leave creation buffered until the first motion/button request. */
    if (wl_display_roundtrip(display) < 0) return 4;
    char command[16];
    unsigned x, y, width, height, button, state;
    while (scanf("%15s", command) == 1) {
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        uint32_t time = now.tv_sec * 1000 + now.tv_nsec / 1000000;
        if (!strcmp(command, "move")) {
            if (scanf("%u %u %u %u", &x, &y, &width, &height) != 4) return 3;
            zwlr_virtual_pointer_v1_motion_absolute(pointer, time, x, y, width, height);
        } else if (!strcmp(command, "button")) {
            if (scanf("%u %u", &button, &state) != 2) return 3;
            zwlr_virtual_pointer_v1_button(pointer, time, button == 1 ? BTN_LEFT : BTN_RIGHT, state);
        } else return 3;
        zwlr_virtual_pointer_v1_frame(pointer);
        if (wl_display_roundtrip(display) < 0) return 4;
    }
    zwlr_virtual_pointer_v1_destroy(pointer);
    zwlr_virtual_pointer_manager_v1_destroy(manager);
    wl_registry_destroy(registry);
    wl_display_flush(display);
    wl_display_disconnect(display);
    return 0;
}
