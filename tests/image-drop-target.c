/* Receives the editor's PNG-file drag in the private smoke-test compositor. */
#include <gtk/gtk.h>
#include <gtk4-layer-shell.h>

static GMainLoop *loop;
static const char *destination;
static int result = 1;
static gboolean dropped(GtkDropTarget *target, const GValue *value,
                         double x, double y, gpointer data) {
    (void)target; (void)x; (void)y; (void)data;
    GdkFileList *list = g_value_get_boxed(value);
    GSList *files = gdk_file_list_get_files(list);
    if (!files) return FALSE;
    GFile *out = g_file_new_for_path(destination);
    GError *error = NULL;
    gboolean copied = g_file_copy(G_FILE(files->data), out, G_FILE_COPY_NONE,
                                  NULL, NULL, NULL, &error);
    if (error) { g_printerr("%s\n", error->message); g_error_free(error); }
    g_object_unref(out);
    if (copied) { result = 0; g_main_loop_quit(loop); }
    return copied;
}
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    destination = argv[1];
    gtk_init();
    GtkWindow *window = GTK_WINDOW(gtk_window_new());
    gtk_layer_init_for_window(window);
    gtk_layer_set_layer(window, GTK_LAYER_SHELL_LAYER_OVERLAY);
    gtk_layer_set_keyboard_mode(window, GTK_LAYER_SHELL_KEYBOARD_MODE_NONE);
    gtk_layer_set_anchor(window, GTK_LAYER_SHELL_EDGE_RIGHT, TRUE);
    gtk_layer_set_anchor(window, GTK_LAYER_SHELL_EDGE_TOP, TRUE);
    gtk_layer_set_margin(window, GTK_LAYER_SHELL_EDGE_RIGHT, 16);
    gtk_layer_set_margin(window, GTK_LAYER_SHELL_EDGE_TOP, 80);
    gtk_window_set_default_size(window, 160, 100);
    GtkWidget *label = gtk_label_new("Drop PNG here");
    gtk_window_set_child(window, label);
    GtkDropTarget *target = gtk_drop_target_new(GDK_TYPE_FILE_LIST, GDK_ACTION_COPY);
    g_signal_connect(target, "drop", G_CALLBACK(dropped), NULL);
    gtk_widget_add_controller(GTK_WIDGET(window), GTK_EVENT_CONTROLLER(target));
    loop = g_main_loop_new(NULL, FALSE);
    gtk_window_present(window);
    g_main_loop_run(loop);
    gtk_window_destroy(window);
    g_main_loop_unref(loop);
    return result;
}
