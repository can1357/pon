from __future__ import annotations

import annotationlib
import dataclasses


class C:
    x: pathlib.Path | None


annotations = annotationlib.get_annotations(C, format=annotationlib.Format.FORWARDREF)
assert annotations == {'x': 'pathlib.Path | None'}


@dataclasses.dataclass
class D:
    path: pathlib.Path | None
    label: str = 'default'


d = D(None)
assert d.path is None
assert d.label == 'default'
assert annotationlib.get_annotations(D, format=annotationlib.Format.FORWARDREF) == {
    'path': 'pathlib.Path | None',
    'label': 'str',
}

print('ok')
