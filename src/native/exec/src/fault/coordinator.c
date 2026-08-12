#if defined(__linux__)
#define _GNU_SOURCE
#endif

#include "../../include/executor.h"

#include <signal.h>
#include <stdint.h>

#if defined(__linux__)
#include <pthread.h>
#include <string.h>
#include <unistd.h>

typedef struct hl_native_fault_process_state {
  pthread_mutex_t lock;
  uint64_t references;
  struct sigaction prior[2];
} hl_native_fault_process_state;

static hl_native_fault_process_state hl_native_fault_process = {
    .lock = PTHREAD_MUTEX_INITIALIZER,
};
static pthread_once_t hl_native_fault_atfork_once = PTHREAD_ONCE_INIT;
static int hl_native_fault_atfork_status;
static const int hl_native_fault_signals[2] = {SIGSEGV, SIGBUS};

static void fault_chain(int signal, siginfo_t *information, void *context,
                        const struct sigaction *prior) {
  uintptr_t disposition = (uintptr_t)prior->sa_handler;
  if (disposition == (uintptr_t)SIG_IGN)
    return;
  if (disposition == (uintptr_t)SIG_DFL) {
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = SIG_DFL;
    sigemptyset(&action.sa_mask);
    (void)sigaction(signal, &action, NULL);
    (void)kill(getpid(), signal);
    return;
  }
  if ((prior->sa_flags & SA_RESETHAND) != 0) {
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = SIG_DFL;
    sigemptyset(&action.sa_mask);
    (void)sigaction(signal, &action, NULL);
  }
  if ((prior->sa_flags & SA_SIGINFO) != 0)
    prior->sa_sigaction(signal, information, context);
  else
    prior->sa_handler(signal);
}

static void fault_dispatch(int signal, siginfo_t *information, void *context) {
  uint64_t address =
      information == NULL ? 0 : (uint64_t)(uintptr_t)information->si_addr;
  uint64_t program = 0;
#if defined(__aarch64__)
  if (context != NULL)
    program = ((ucontext_t *)context)->uc_mcontext.pc;
#elif defined(__x86_64__)
  if (context != NULL)
    program = ((ucontext_t *)context)->uc_mcontext.gregs[REG_RIP];
#endif
  if (program != 0 && hl_native_fault_thread_prepare(program, address, context))
    return;
  size_t index = signal == SIGBUS ? 1u : 0u;
  fault_chain(signal, information, context,
              &hl_native_fault_process.prior[index]);
}

static void fault_fork_prepare(void) {
  (void)pthread_mutex_lock(&hl_native_fault_process.lock);
}

static void fault_fork_parent(void) {
  (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
}

static void fault_fork_child(void) {
  (void)hl_native_fault_thread_after_fork_child();
  (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
}

static void fault_atfork_install(void) {
  hl_native_fault_atfork_status =
      pthread_atfork(fault_fork_prepare, fault_fork_parent, fault_fork_child);
}

hl_native_status hl_native_fault_process_acquire(void) {
  (void)pthread_once(&hl_native_fault_atfork_once, fault_atfork_install);
  if (hl_native_fault_atfork_status != 0)
    return HL_NATIVE_PLATFORM;
  if (pthread_mutex_lock(&hl_native_fault_process.lock) != 0)
    return HL_NATIVE_PLATFORM;
  if (hl_native_fault_process.references != 0) {
    hl_native_fault_process.references++;
    (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
    return HL_NATIVE_OK;
  }
  size_t count = 0;
  for (; count < 2; count++) {
    int signal = hl_native_fault_signals[count];
    if (sigaction(signal, NULL, &hl_native_fault_process.prior[count]) != 0)
      break;
    struct sigaction installed;
    memset(&installed, 0, sizeof(installed));
    installed.sa_sigaction = fault_dispatch;
    installed.sa_mask = hl_native_fault_process.prior[count].sa_mask;
    installed.sa_flags = SA_SIGINFO | SA_ONSTACK |
                         (hl_native_fault_process.prior[count].sa_flags &
                          (SA_NODEFER | SA_RESTART));
    if (sigaction(signal, &installed, NULL) != 0)
      break;
  }
  if (count != 2) {
    while (count > 0) {
      count--;
      (void)sigaction(hl_native_fault_signals[count],
                      &hl_native_fault_process.prior[count], NULL);
    }
    (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
    return HL_NATIVE_PLATFORM;
  }
  hl_native_fault_process.references = 1;
  (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
  return HL_NATIVE_OK;
}

hl_native_status hl_native_fault_process_release(void) {
  if (pthread_mutex_lock(&hl_native_fault_process.lock) != 0)
    return HL_NATIVE_PLATFORM;
  if (hl_native_fault_process.references == 0) {
    (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
    return HL_NATIVE_STATE;
  }
  if (--hl_native_fault_process.references != 0) {
    (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
    return HL_NATIVE_OK;
  }
  for (size_t index = 0; index < 2; index++)
    if (sigaction(hl_native_fault_signals[index],
                  &hl_native_fault_process.prior[index], NULL) != 0) {
      hl_native_fault_process.references = 1;
      (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
      return HL_NATIVE_PLATFORM;
    }
  (void)pthread_mutex_unlock(&hl_native_fault_process.lock);
  return HL_NATIVE_OK;
}

#else
hl_native_status hl_native_fault_process_acquire(void) {
  return HL_NATIVE_PLATFORM;
}
hl_native_status hl_native_fault_process_release(void) {
  return HL_NATIVE_PLATFORM;
}
#endif
