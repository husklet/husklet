    case 283:
        switch ((int)a0) {
        case 0: // CMD_QUERY -> bitmask of supported commands (every command we accept below)
            G_RET(c) = (1u << 0) | (1u << 1) | (1u << 2) | (1u << 3) | (1u << 4) | (1u << 5) | (1u << 6);
            break;
        case 1:  // CMD_GLOBAL
        case 2:  // CMD_GLOBAL_EXPEDITED
        case 8:  // CMD_PRIVATE_EXPEDITED
        case 32: // CMD_PRIVATE_EXPEDITED_SYNC_CORE
            atomic_thread_fence(memory_order_seq_cst);
            G_RET(c) = 0;
            break;
        case 4:           // CMD_REGISTER_GLOBAL_EXPEDITED
        case 16:          // CMD_REGISTER_PRIVATE_EXPEDITED
        case 64:          // CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE
            G_RET(c) = 0; // arm the expedited fast path -> nothing to do in this coherent DBT
            break;
        default: G_RET(c) = (uint64_t)(-EINVAL); break;
        }
        break;
