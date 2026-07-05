#ifndef PON_PYTHREAD_H
#define PON_PYTHREAD_H

/* CPython `pythread.h` compatibility shim over POSIX threads.
 *
 * Cython-generated C includes this header unconditionally; the lock surface
 * below is the subset extensions actually touch (PyThread_type_lock plus
 * allocate/free/acquire/release and thread idents). Locks are plain
 * pthread mutexes allocated on the C heap — no Pon runtime involvement.
 */

#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>

typedef void *PyThread_type_lock;

#define WAIT_LOCK 1
#define NOWAIT_LOCK 0

typedef enum PyLockStatus {
    PY_LOCK_FAILURE = 0,
    PY_LOCK_ACQUIRED = 1,
    PY_LOCK_INTR = 2
} PyLockStatus;

#define PY_TIMEOUT_T long long

static inline PyThread_type_lock PyThread_allocate_lock(void) {
    pthread_mutex_t *lock = (pthread_mutex_t *)malloc(sizeof(pthread_mutex_t));
    if (lock == NULL) {
        return NULL;
    }
    if (pthread_mutex_init(lock, NULL) != 0) {
        free(lock);
        return NULL;
    }
    return (PyThread_type_lock)lock;
}

static inline void PyThread_free_lock(PyThread_type_lock lock) {
    if (lock == NULL) {
        return;
    }
    pthread_mutex_destroy((pthread_mutex_t *)lock);
    free(lock);
}

static inline int PyThread_acquire_lock(PyThread_type_lock lock, int waitflag) {
    if (lock == NULL) {
        return 0;
    }
    if (waitflag) {
        return pthread_mutex_lock((pthread_mutex_t *)lock) == 0;
    }
    return pthread_mutex_trylock((pthread_mutex_t *)lock) == 0;
}

/* Timeouts degrade to blocking/try semantics: extensions in this build only
 * pass -1 (block) or 0 (try). */
static inline PyLockStatus PyThread_acquire_lock_timed(PyThread_type_lock lock, PY_TIMEOUT_T microseconds, int intr_flag) {
    (void)intr_flag;
    if (microseconds == 0) {
        return PyThread_acquire_lock(lock, NOWAIT_LOCK) ? PY_LOCK_ACQUIRED : PY_LOCK_FAILURE;
    }
    return PyThread_acquire_lock(lock, WAIT_LOCK) ? PY_LOCK_ACQUIRED : PY_LOCK_FAILURE;
}

static inline void PyThread_release_lock(PyThread_type_lock lock) {
    if (lock != NULL) {
        pthread_mutex_unlock((pthread_mutex_t *)lock);
    }
}

static inline unsigned long PyThread_get_thread_ident(void) {
    return (unsigned long)(uintptr_t)pthread_self();
}

#endif /* PON_PYTHREAD_H */
