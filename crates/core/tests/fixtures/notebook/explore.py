# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "marimo",
#     "numpy==2.3.4",
#     "polars==1.34.0",
# ]
# ///

import marimo

__generated_with = "0.17.2"
app = marimo.App(width="medium")

with app.setup:
    import marimo as mo
    import numpy as np


@app.cell(hide_code=True)
def _():
    mo.md(r"""
    # Explore the sweep

    Readout fidelity against drive amplitude, $F = 1 - \frac{p_{01} + p_{10}}{2}$.
    """)
    return


@app.function
def fidelity(p01, p10):
    """Readout fidelity from the two error rates."""
    return 1 - (p01 + p10) / 2


@app.cell
def _():
    import polars as pl

    return (pl,)


@app.cell
def _():
    amps = np.linspace(0.1, 1.0, 10)
    errors = np.exp(-3 * amps) / 4
    return amps, errors


@app.cell
def _(amps, errors, pl):
    amplitudes = amps
    readout_errors = errors
    fidelity_threshold = 0.9
    calibration_table_by_qubit = pl.DataFrame({"amp": amps, "error": errors})
    return (
        amplitudes,
        calibration_table_by_qubit,
        fidelity_threshold,
        readout_errors,
    )


@app.cell
def _(
    amplitudes,
    calibration_table_by_qubit,
    fidelity_threshold,
    readout_errors,
):
    passing = amplitudes[fidelity(readout_errors, readout_errors) > fidelity_threshold]
    best_amplitude = passing.max()
    calibration_table_by_qubit.head()
    return (best_amplitude,)


@app.cell
def _(best_amplitude):
    mo.md(f"The best amplitude is **{best_amplitude:.2f}**.")
    return


@app.cell
def _():
    shots = mo.ui.slider(128, 4096, step=128, value=1024, label="Shots")
    shots
    return


app._unparsable_cell(
    r"""
    def unfinished(amps:
    """,
    name="_"
)


@app.cell
def _():
    return


if __name__ == "__main__":
    app.run()
