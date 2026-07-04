import re
for expr in ["{1}.symmetric_difference([1,1,2])", "frozenset({1}).symmetric_difference((1,1,2))"]:
    print(eval(expr))
s={1,2,3}
s.symmetric_difference_update([2,2,4])
print(sorted(s))
m=re.match(r'(a)?','')
for call in (lambda: m[2], lambda: m.span(2), lambda: m.start('x')):
    try:
        call()
    except IndexError as e:
        print(type(e).__name__, e)
