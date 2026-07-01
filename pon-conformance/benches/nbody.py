def run():
    x0 = 0 - 1
    y0 = 0
    vx0 = 0
    vy0 = 0

    x1 = 2
    y1 = 0 - 3
    vx1 = 0
    vy1 = 0

    x2 = 4
    y2 = 5
    vx2 = 0
    vy2 = 0

    for step in range(800):
        if x0 < x1:
            vx0 = vx0 + 1
            vx1 = vx1 - 1
        if x0 > x1:
            vx0 = vx0 - 1
            vx1 = vx1 + 1
        if y0 < y1:
            vy0 = vy0 + 1
            vy1 = vy1 - 1
        if y0 > y1:
            vy0 = vy0 - 1
            vy1 = vy1 + 1

        if x0 < x2:
            vx0 = vx0 + 1
            vx2 = vx2 - 1
        if x0 > x2:
            vx0 = vx0 - 1
            vx2 = vx2 + 1
        if y0 < y2:
            vy0 = vy0 + 1
            vy2 = vy2 - 1
        if y0 > y2:
            vy0 = vy0 - 1
            vy2 = vy2 + 1

        if x1 < x2:
            vx1 = vx1 + 1
            vx2 = vx2 - 1
        if x1 > x2:
            vx1 = vx1 - 1
            vx2 = vx2 + 1
        if y1 < y2:
            vy1 = vy1 + 1
            vy2 = vy2 - 1
        if y1 > y2:
            vy1 = vy1 - 1
            vy2 = vy2 + 1

        x0 = x0 + vx0
        y0 = y0 + vy0
        x1 = x1 + vx1
        y1 = y1 + vy1
        x2 = x2 + vx2
        y2 = y2 + vy2

    checksum = x0 * 3 + y0 * 5 + vx0 * 7 + vy0 * 11
    checksum = checksum + x1 * 13 + y1 * 17 + vx1 * 19 + vy1 * 23
    checksum = checksum + x2 * 29 + y2 * 31 + vx2 * 37 + vy2 * 41
    return checksum


print("nbody")
print(run())
