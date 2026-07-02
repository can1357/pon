cases = [
    ("(1,2)<(1,3)", lambda: (1, 2) < (1, 3)),
    ("[1,2]<[1,3]", lambda: [1, 2] < [1, 3]),
    ("(1,2)<(1,2,0)", lambda: (1, 2) < (1, 2, 0)),
    ("(1,2,0)<=(1,2)", lambda: (1, 2, 0) <= (1, 2)),
    ("(1,2)>=(1,2)", lambda: (1, 2) >= (1, 2)),
    ("[1,2]>[1,1,9]", lambda: [1, 2] > [1, 1, 9]),
    ("((1,2),(3,))<((1,2),(3,1))", lambda: ((1, 2), (3,)) < ((1, 2), (3, 1))),
    ("[(1,'a')]<[(1,'b')]", lambda: [(1, 'a')] < [(1, 'b')]),
    ("('a','b')<('a','c')", lambda: ('a', 'b') < ('a', 'c')),
]


def probe(label, thunk):
    try:
        print(label, "->", thunk())
    except Exception as exc:
        print(label, "-> RAISED", type(exc).__name__, exc)


for label, thunk in cases:
    probe(label, thunk)

probe("list<tuple", lambda: [1, 2] < (1, 3))
probe("tuple<list", lambda: (1, 2) < [1, 3])

probe("sorted tuples", lambda: sorted([(3, 'c'), (1, 'a'), (2, 'b')]))
probe("list.sort tuples", lambda: [x.sort() for x in [[(3, 'c'), (1, 'a'), (2, 'b')]]])
probe("min tuples", lambda: min((3, 'c'), (1, 'a')))


class TupSub(tuple):
    pass


class ListSub(list):
    pass


probe("TupSub<TupSub", lambda: TupSub((1, 2)) < TupSub((1, 3)))
probe("TupSub<tuple", lambda: TupSub((1, 2)) < (1, 3))
probe("tuple<TupSub", lambda: (1, 2) < TupSub((1, 3)))
probe("ListSub<list", lambda: ListSub([1, 2]) < [1, 3])
probe("sorted TupSub", lambda: sorted([TupSub((2, 'b')), TupSub((1, 'a'))]))

import collections
Pt = collections.namedtuple('Pt', ['x', 'y'])
probe("namedtuple<namedtuple", lambda: Pt(1, 2) < Pt(1, 3))
probe("sorted namedtuples", lambda: sorted([Pt(2, 'b'), Pt(1, 'a')]))

import heapq
probe("heapq.nsmallest", lambda: heapq.nsmallest(2, [(3, 'c'), (1, 'a'), (2, 'b')]))
probe("heapq.nlargest", lambda: heapq.nlargest(2, [(3, 'c'), (1, 'a'), (2, 'b')]))
probe("heapify+pops", lambda: (lambda h: (heapq.heapify(h), [heapq.heappop(h) for _ in range(3)])[1])([(3, 'c'), (1, 'a'), (2, 'b')]))
