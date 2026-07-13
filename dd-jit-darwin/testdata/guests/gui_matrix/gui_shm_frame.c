#include "gui_probe_wayland.h"

int main(void) {
    struct gp_conn c;
    struct gp_events ev;
    memset(&ev, 0, sizeof(ev));
    if (gp_connect(&c) != 0) return 1;
    if (gp_xdg_setup(&c, &ev, "gui_shm_frame", 1) != 1) {
        printf("gui_shm_frame configure=0\n");
        return 2;
    }
    if (gp_commit_shm_frame(&c, 96, 64, GP_FRAME) != 0) return 3;
    int ok = gp_wait_frame_release(&c, &ev, 1500);
    printf("gui_shm_frame configure=%u release=%d frame_done=%d ok=%d\n",
           ev.xdg_configure_serial, ev.got_buffer_release, ev.got_frame_done, ok == 1);
    return ok == 1 ? 0 : 4;
}
