"""Readout fidelity of each qubit, from its calibration shots."""

# Shots read as 0 and as 1, prepared in |0> and in |1>.
COUNTS = {
    "q0": ((962, 38), (51, 949)),
    "q1": ((941, 59), (87, 913)),
    "q2": ((978, 22), (40, 960)),
    "q3": ((903, 97), (122, 878)),
}
LIMIT = 0.95


def fidelity(zero, one):
    """The mean chance of reading each state right."""
    p0 = zero[0] / sum(zero)
    p1 = one[1] / sum(one)
    return (p0 + p1) / 2


def main():
    print(f"{len(COUNTS)} qubits, limit {LIMIT}")
    weak = []
    for qubit, (zero, one) in COUNTS.items():
        f = fidelity(zero, one)
        print(f"{qubit}  {f:.4f}")
        if f < LIMIT:
            weak.append(qubit)
    print("below the limit:", ", ".join(weak))


if __name__ == "__main__":
    main()
