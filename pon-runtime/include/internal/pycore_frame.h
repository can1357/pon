#ifndef PON_INTERNAL_PYCORE_FRAME_H
#define PON_INTERNAL_PYCORE_FRAME_H

/* Cython includes CPython's internal frame header on 3.11+ after defining
 * Py_BUILD_CORE, but its selected Pon branch only needs the public concrete
 * PyFrameObject surface from frameobject.h.
 */
#ifndef PON_FRAMEOBJECT_H
#include "frameobject.h"
#endif

#endif /* PON_INTERNAL_PYCORE_FRAME_H */
