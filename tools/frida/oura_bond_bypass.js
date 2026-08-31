setImmediate(function () {
  Java.perform(function () {
    function install() {
      try {
        const Connector = Java.use("com.ouraring.ourakit.RxBleRingBondConnector");
        const Success = Java.use("com.ouraring.ourakit.RingBondConnector$Result$RingBondSuccess");

        Connector.isBonded.overload("java.lang.String").implementation = function (address) {
          console.log("[oura] isBonded(" + address + ") => true");
          return true;
        };

        Connector.connect.overload("java.lang.String", "kotlin.jvm.functions.Function1").implementation = function (address, listener) {
          console.log("[oura] bypass bond connect(" + address + ")");
          listener.invoke(Success.$new(address));
        };

        console.log("[oura] bond bypass installed");
      } catch (e) {
        console.log("[oura] hook install failed: " + e);
        setTimeout(install, 1000);
      }
    }

    install();
  });
});
