/* Deterministic colored desktop for region-smoke.py's private compositor. */
#include <gtk/gtk.h>
#include <gtk4-layer-shell.h>

static void draw(GtkDrawingArea *area, cairo_t *cr, int width, int height, gpointer data) {
    (void)area;
    double blue = GPOINTER_TO_INT(data) ? 0.6 : 0.2;
    for (int y = 0; y < height; y += 20) {
        for (int x = 0; x < width; x += 20) {
            cairo_set_source_rgb(cr, 0.15 + 0.7 * x / width, 0.15 + 0.7 * y / height, blue);
            cairo_rectangle(cr, x, y, 20, 20);
            cairo_fill(cr);
        }
    }
}

int main(void) {
    gtk_init();
    if (!gtk_layer_is_supported()) return 1;
    GListModel *monitors = gdk_display_get_monitors(gdk_display_get_default());
    for (guint i = 0; i < g_list_model_get_n_items(monitors); i++) {
        GdkMonitor *monitor = g_list_model_get_item(monitors, i);
        GtkWindow *window = GTK_WINDOW(gtk_window_new());
        gtk_layer_init_for_window(window);
        gtk_layer_set_layer(window, GTK_LAYER_SHELL_LAYER_BACKGROUND);
        gtk_layer_set_monitor(window, monitor);
        gtk_layer_set_exclusive_zone(window, -1);
        for (int edge = 0; edge < GTK_LAYER_SHELL_EDGE_ENTRY_NUMBER; edge++)
            gtk_layer_set_anchor(window, edge, TRUE);
        GtkWidget *area = gtk_drawing_area_new();
        gtk_drawing_area_set_draw_func(GTK_DRAWING_AREA(area), draw, GINT_TO_POINTER(i), NULL);
        gtk_window_set_child(window, area);
        gtk_window_present(window);
        g_object_unref(monitor);
    }
    GMainLoop *loop = g_main_loop_new(NULL, FALSE);
    g_main_loop_run(loop);
    g_main_loop_unref(loop);
    return 0;
}
