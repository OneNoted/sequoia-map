package io.iris.reporter;

import org.junit.jupiter.api.Test;

import java.util.Arrays;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class SeasonScalarMathTest {
    @Test
    void weightedUnitsMatchesRegressionLadder() {
        // 3.00 + 2.00 + 1.00 + 0.90 + 0.80 = 7.70
        assertClose(SeasonScalarMath.weightedUnits(5), 7.7);
    }

    @Test
    void weightedUnitsClampsAtTwentyThreePlus() {
        double units22 = SeasonScalarMath.weightedUnits(22);
        double units23 = SeasonScalarMath.weightedUnits(23);
        assertClose(units23 - units22, 0.2);
    }

    @Test
    void weightedUnitsMatchesFullRegressionLadder() {
        double[] multipliers = {
            3.00, 2.00, 1.00, 0.90, 0.80, 0.75, 0.75, 0.70, 0.70, 0.65, 0.65, 0.60,
            0.60, 0.55, 0.55, 0.50, 0.45, 0.40, 0.35, 0.30, 0.25, 0.20
        };
        double expected = Arrays.stream(multipliers).sum();
        assertClose(SeasonScalarMath.weightedUnits(22), expected);
        assertClose(SeasonScalarMath.weightedUnits(100) - expected, (100 - 22) * 0.20);
    }

    @Test
    void scalarInferenceHelpersRecoverExpectedValues() {
        int territories = 58;

        double weightedExpected = 2.75;
        double weightedHourly = SeasonScalarMath.BASE_HOURLY_SR
            * weightedExpected
            * SeasonScalarMath.weightedUnits(territories);
        assertClose(
            SeasonScalarMath.scalarWeightedFromSrPerHour(weightedHourly, territories),
            weightedExpected
        );

        double rawExpected = 1.8;
        double rawHourly = SeasonScalarMath.BASE_HOURLY_SR * rawExpected * territories;
        assertClose(
            SeasonScalarMath.scalarRawFromSrPerHour(rawHourly, territories),
            rawExpected
        );
    }

    @Test
    void invalidInputsYieldNan() {
        assertTrue(Double.isNaN(SeasonScalarMath.scalarWeightedFromSrPerHour(1000.0, 0)));
        assertTrue(Double.isNaN(SeasonScalarMath.scalarRawFromSrPerHour(1000.0, 0)));
        assertTrue(Double.isNaN(SeasonScalarMath.scalarWeightedFromSrPerHour(-1.0, 5)));
        assertTrue(Double.isNaN(SeasonScalarMath.scalarRawFromSrPerHour(-1.0, 5)));
        assertTrue(Double.isNaN(SeasonScalarMath.scalarWeightedFromSrPerHour(Double.POSITIVE_INFINITY, 5)));
        assertTrue(Double.isNaN(SeasonScalarMath.scalarRawFromSrPerHour(Double.NaN, 5)));
    }

    private static void assertClose(double actual, double expected) {
        assertEquals(expected, actual, 1e-9);
    }
}
