//! Native Windows network-cost/power observation, owned by the offline worker.
use crate::download::offline::Environment;
use windows::Foundation::EventHandler;
use windows::Networking::Connectivity::{
    NetworkConnectivityLevel, NetworkCostType, NetworkInformation, NetworkStatusChangedEventHandler,
};
use windows::System::Power::PowerManager;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};

pub(super) struct Monitor {
    network: Option<i64>,
    power: Option<i64>,
}

impl Monitor {
    pub(super) fn new() -> windows::core::Result<Self> {
        // This dedicated worker owns the apartment and drops every registration
        // on the same thread before balancing RoInitialize.
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        let worker = std::thread::current();
        let network = NetworkInformation::NetworkStatusChanged(
            &NetworkStatusChangedEventHandler::new(move |_| {
                worker.unpark();
                Ok(())
            }),
        )
        .ok();
        let worker = std::thread::current();
        let power = PowerManager::PowerSupplyStatusChanged(&EventHandler::<
            windows::core::IInspectable,
        >::new(move |_, _| {
            worker.unpark();
            Ok(())
        }))
        .ok();
        Ok(Self { network, power })
    }

    pub(super) fn probe(&self) -> Environment {
        let mut environment = Self::network_environment().unwrap_or_default();
        let mut power = SYSTEM_POWER_STATUS::default();
        if unsafe { GetSystemPowerStatus(&mut power) }.is_ok() {
            // ACLineStatus=255 and BatteryFlag=255 mean unknown, not permission.
            environment.charging = power.ACLineStatus == 1 || power.BatteryFlag == 128;
        }
        environment
    }

    fn network_environment() -> windows::core::Result<Environment> {
        let profile = NetworkInformation::GetInternetConnectionProfile()?;
        let cost = profile.GetConnectionCost()?;
        let interface = profile.NetworkAdapter()?.IanaInterfaceType()?;
        Ok(Environment {
            connected: profile.GetNetworkConnectivityLevel()?
                == NetworkConnectivityLevel::InternetAccess,
            wifi_or_ethernet: matches!(interface, 6 | 71),
            unmetered: cost.NetworkCostType()? == NetworkCostType::Unrestricted
                && !cost.Roaming()?
                && !cost.OverDataLimit()?,
            charging: false,
        })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        if let Some(token) = self.network.take() {
            let _ = NetworkInformation::RemoveNetworkStatusChanged(token);
        }
        if let Some(token) = self.power.take() {
            let _ = PowerManager::RemovePowerSupplyStatusChanged(token);
        }
        unsafe {
            RoUninitialize();
        }
    }
}
