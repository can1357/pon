#include <Python.h>

static const PyPonCapi *pypon_capi;

int PyPon_SetCapi(const PyPonCapi *api) {
    pypon_capi = api;
    return api == 0 ? -1 : 0;
}

const PyPonCapi *PyPon_GetCapi(void) {
    return pypon_capi;
}
