package io.iris.reporter;

/**
 * Mirrors shared season scalar math used server-side for diagnostics.
 */
public final class SeasonScalarMath {
    public static final double BASE_HOURLY_SR = 120.0;

    private static final double[] REGRESSION_MULTIPLIERS = new double[] {
        3.00, 2.00, 1.00, 0.90, 0.80, 0.75, 0.75, 0.70, 0.70, 0.65, 0.65, 0.60,
        0.60, 0.55, 0.55, 0.50, 0.45, 0.40, 0.35, 0.30, 0.25, 0.20
    };

    private SeasonScalarMath() {
    }

    public static double weightedUnits(int territoryCount) {
        if (territoryCount <= 0) {
            return 0.0;
        }

        double total = 0.0;
        for (int idx = 0; idx < territoryCount; idx += 1) {
            total += regressionMultiplier(idx);
        }
        return total;
    }

    public static double scalarWeightedFromSrPerHour(double srPerHour, int territoryCount) {
        if (!Double.isFinite(srPerHour) || srPerHour <= 0.0) {
            return Double.NaN;
        }
        double units = weightedUnits(territoryCount);
        if (units <= 0.0) {
            return Double.NaN;
        }
        return srPerHour / (BASE_HOURLY_SR * units);
    }

    public static double scalarRawFromSrPerHour(double srPerHour, int territoryCount) {
        if (!Double.isFinite(srPerHour) || srPerHour <= 0.0 || territoryCount <= 0) {
            return Double.NaN;
        }
        return srPerHour / (BASE_HOURLY_SR * territoryCount);
    }

    private static double regressionMultiplier(int idx) {
        if (idx < 0) {
            return REGRESSION_MULTIPLIERS[0];
        }
        int clamped = Math.min(idx, REGRESSION_MULTIPLIERS.length - 1);
        return REGRESSION_MULTIPLIERS[clamped];
    }
}
