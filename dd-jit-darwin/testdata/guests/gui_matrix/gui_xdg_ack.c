#include "gui_probe_wayland.h"

int main(void) {
    struct gp_conn c;
    struct gp_events ev;
    memset(&ev, 0, sizeof(ev));
    if (gp_connect(&c) != 0) return 1;
    int r = gp_xdg_setup(&c, &ev, "gui_xdg_ack", 0);
    if (r != 1) {
        printf("gui_xdg_ack configure=0 ack=0\n");
        return 2;
    }
    gp_send_empty(&c, GP_SURFACE, 6);
    if (gp_flush(&c) != 0) return 3;
    gp_drain(&c, &ev, 100);
    printf("gui_xdg_ack configure=%u ack=1 nil_recommit=1 toplevel=%d\n",
           ev.xdg_configure_serial, ev.got_toplevel_configure);
    return 0;
}
