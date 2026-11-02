//! 校园网 Portal 发现与认证、本机出口 IP 探测及自助服务设备管理接口。

pub use zzu_core::network::{
    AuthResult, OnlineDevice, PortalClient, PortalEncryption, PortalInfo, SelfServiceClient,
    discover_portal_info, discover_portal_info_with_trust_roots, local_ip,
};
