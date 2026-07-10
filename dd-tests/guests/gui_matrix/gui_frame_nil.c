#include "gui_probe_wayland.h"

int main(void) {
    struct gp_conn c;
    struct gp_events ev;
    memset(&ev, 0, sizeof(ev));
    if (gp_connect(&c) != 0) return 1;
    if (gp_xdg_setup(&c, &ev, "gui_frame_nil", 0) != 1) {
        printf("gui_frame_nil configure=0\n");
        return 2;
    }

    uint32_t frame = GP_FRAME;
    gp_send_u32(&c, GP_SURFACE, 3, &frame, 1);
    gp_send_empty(&c, GP_SURFACE, 6);
    if (gp_flush(&c) != 0) return 3;
    gp_drain(&c, &ev, 350);
    printf("gui_frame_nil configure=%u frame_done_without_buffer=%d\n",
           ev.xdg_configure_serial, ev.got_frame_done);
    return ev.got_frame_done ? 4 : 0;
}
