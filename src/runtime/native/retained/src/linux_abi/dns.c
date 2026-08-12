#define _POSIX_C_SOURCE 200809L
#include "dns.h"

/* Warming the host resolver is a prewarm, not a service: it resolves one throwaway name so that the lazy
 * work a first getaddrinfo() performs -- loading the resolver's shared objects, reading the system
 * configuration, and on macOS initializing the Foundation string classes underneath it -- has already
 * happened by the time the guest is confined and can no longer reach any of it.
 *
 * Windows has no <netdb.h> and therefore no getaddrinfo() outside Winsock, which the portable layers do not
 * initialize. There is nothing to prewarm here and nothing this can approximate: the guest-facing resolver
 * path for this host does not exist yet. The absence is the whole implementation below -- deliberately a
 * no-op rather than a degraded resolve, because a prewarm has no result a caller can observe and a partial
 * one would only move the same first-call cost, never a correctness answer. */
#if defined(_WIN32)

void hl_linux_dns_prepare(void) {
}

#else

#include <netdb.h>
#include <pthread.h>
#if defined(__APPLE__)
#include <dlfcn.h>
#include <string.h>
#endif

static pthread_once_t dns_preparation = PTHREAD_ONCE_INIT;

#if defined(__APPLE__)
static void prepare_foundation_strings(void) {
    typedef void *(*class_lookup)(const char *);
    typedef void *(*selector_lookup)(const char *);
    typedef void *(*string_create)(void *, void *, const char *);
    typedef void *(*mutable_string_create)(void *, void *, unsigned long);
    typedef void *(*class_initialize)(void *, void *);

    void *foundation = dlopen("/System/Library/Frameworks/Foundation.framework/Foundation", RTLD_NOW | RTLD_LOCAL);
    void *runtime = dlopen("/usr/lib/libobjc.A.dylib", RTLD_NOW | RTLD_LOCAL);
    if (foundation == NULL || runtime == NULL) return;

    void *class_symbol = dlsym(runtime, "objc_getClass");
    void *selector_symbol = dlsym(runtime, "sel_registerName");
    void *message_symbol = dlsym(runtime, "objc_msgSend");
    class_lookup get_class = NULL;
    selector_lookup get_selector = NULL;
    string_create create_string = NULL;
    mutable_string_create create_mutable_string = NULL;
    class_initialize initialize_class = NULL;
    memcpy(&get_class, &class_symbol, sizeof(get_class));
    memcpy(&get_selector, &selector_symbol, sizeof(get_selector));
    memcpy(&create_string, &message_symbol, sizeof(create_string));
    memcpy(&create_mutable_string, &message_symbol, sizeof(create_mutable_string));
    memcpy(&initialize_class, &message_symbol, sizeof(initialize_class));
    if (get_class == NULL || get_selector == NULL || create_string == NULL || create_mutable_string == NULL ||
        initialize_class == NULL)
        return;

    void *class_selector = get_selector("class");
    const char *concrete_classes[] = {
        "NSString", "NSMutableString", "__NSCFConstantString", "__NSCFString", "NSTaggedPointerString",
    };
    if (class_selector != NULL)
        for (size_t index = 0; index < sizeof(concrete_classes) / sizeof(concrete_classes[0]); ++index) {
            void *class_object = get_class(concrete_classes[index]);
            if (class_object != NULL) (void)initialize_class(class_object, class_selector);
        }
    void *string_class = get_class("NSString");
    void *constructor = get_selector("stringWithUTF8String:");
    if (string_class != NULL && constructor != NULL) (void)create_string(string_class, constructor, "localhost");
    void *mutable_string_class = get_class("NSMutableString");
    void *mutable_constructor = get_selector("stringWithCapacity:");
    if (mutable_string_class != NULL && mutable_constructor != NULL)
        (void)create_mutable_string(mutable_string_class, mutable_constructor, 16);
}
#endif

static void prepare_host_resolver(void) {
    struct addrinfo hints = {0};
    struct addrinfo *result = NULL;
#if defined(__APPLE__)
    prepare_foundation_strings();
#endif
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo("localhost", NULL, &hints, &result) == 0) freeaddrinfo(result);
}

void hl_linux_dns_prepare(void) {
    (void)pthread_once(&dns_preparation, prepare_host_resolver);
}

#endif
