import numpy as np
import matplotlib.pyplot as plt

def plot_xcorr(signal1, signal2, figname):
    # Compute cross-correlation
    cross_corr = np.correlate(signal1, signal2, mode='full')
    lags = np.arange(-len(signal1) + 1, len(signal1))

    cross_corr_normalized = cross_corr / np.max(np.abs(cross_corr))
    with plt.xkcd():
        # Plot the two signals
        plt.figure(figsize=(12, 6))

        plt.subplot(2, 1, 1)
        plt.plot(t, signal1, label='Signal 1')
        plt.plot(t, signal2, label='Signal 2', linestyle='--')
        plt.title('Signals')
        plt.xlabel('Time')
        plt.ylabel('Amplitude')

        # Plot the cross-correlation
        plt.subplot(2, 1, 2)
        plt.plot(lags, cross_corr_normalized)
        plt.axvline(x=0, color='r', linestyle='--')
        plt.title('Cross-Correlation between Signal 1 and Signal 2')
        plt.xlabel('Lag')
        plt.ylabel('Cross-correlation')

        plt.tight_layout()
        plt.savefig(figname, format="svg")


# Generate two signals with some similarity and a slight time lag
t = np.linspace(0, 10, 1000)  # time vector
signal1 = np.sin(2 * np.pi * 1 * t)  # base signal, 1 Hz sine wave
signal2 = np.sin(2 * np.pi * 1 * (t - 0.1))  # delayed version of signal1

plot_xcorr(signal1, signal2, "similar_xcorr.svg")

signal2 = np.sin(2 * np.pi * 1 * (t - 0.5))  # delayed version of signal1

plot_xcorr(signal1, signal2, "dissimilar_xcorr.svg")
