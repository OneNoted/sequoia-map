package io.iris.reporter;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class IrisAutoUpdaterVersionTest {
    @Test
    void parseSupportsIrisTagAndVPrefixes() {
        IrisAutoUpdater.SemanticVersion fromIrisTag = IrisAutoUpdater.parseSemanticVersion("iris-v1.2.3");
        IrisAutoUpdater.SemanticVersion fromVPrefix = IrisAutoUpdater.parseSemanticVersion("v1.2.3");
        IrisAutoUpdater.SemanticVersion plain = IrisAutoUpdater.parseSemanticVersion("1.2.3");

        assertNotNull(fromIrisTag);
        assertNotNull(fromVPrefix);
        assertNotNull(plain);
        assertEquals(0, fromIrisTag.compareTo(fromVPrefix));
        assertEquals(0, fromVPrefix.compareTo(plain));
    }

    @Test
    void buildMetadataDoesNotChangePrecedence() {
        IrisAutoUpdater.SemanticVersion left = IrisAutoUpdater.parseSemanticVersion("1.2.3+abc");
        IrisAutoUpdater.SemanticVersion right = IrisAutoUpdater.parseSemanticVersion("1.2.3+def");

        assertNotNull(left);
        assertNotNull(right);
        assertEquals(0, left.compareTo(right));
    }

    @Test
    void manifestVersionAcceptsBuildMetadataWhenReleaseSemverMatches() {
        IrisAutoUpdater.SemanticVersion releaseVersion = IrisAutoUpdater.parseSemanticVersion("0.1.2");
        assertNotNull(releaseVersion);
        assertTrue(IrisAutoUpdater.isManifestVersionCompatible(releaseVersion, "0.1.2+1_21_11"));
    }

    @Test
    void manifestVersionRejectsDifferentSemverCore() {
        IrisAutoUpdater.SemanticVersion releaseVersion = IrisAutoUpdater.parseSemanticVersion("0.1.2");
        assertNotNull(releaseVersion);
        assertFalse(IrisAutoUpdater.isManifestVersionCompatible(releaseVersion, "0.1.3+1_21_11"));
    }

    @Test
    void prereleaseAndPatchOrderingFollowSemverRules() {
        IrisAutoUpdater.SemanticVersion alpha1 = IrisAutoUpdater.parseSemanticVersion("1.2.3-alpha.1");
        IrisAutoUpdater.SemanticVersion alpha2 = IrisAutoUpdater.parseSemanticVersion("1.2.3-alpha.2");
        IrisAutoUpdater.SemanticVersion stable = IrisAutoUpdater.parseSemanticVersion("1.2.3");
        IrisAutoUpdater.SemanticVersion nextPatch = IrisAutoUpdater.parseSemanticVersion("1.2.4");

        assertNotNull(alpha1);
        assertNotNull(alpha2);
        assertNotNull(stable);
        assertNotNull(nextPatch);

        assertTrue(alpha1.compareTo(alpha2) < 0);
        assertTrue(alpha2.compareTo(stable) < 0);
        assertTrue(nextPatch.compareTo(stable) > 0);
    }

    @Test
    void invalidVersionReturnsNull() {
        assertNull(IrisAutoUpdater.parseSemanticVersion("not-a-version"));
        assertNull(IrisAutoUpdater.parseSemanticVersion("1.2"));
        assertNull(IrisAutoUpdater.parseSemanticVersion(""));
    }
}
