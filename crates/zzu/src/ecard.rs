//! 校园卡会话、余额查询、网页充值下单以及宿舍电费查询与充值接口。

pub use zzu_core::ecard::{
    EcardClient, PaymentAction, PaymentChannel, PaymentContext, PaymentDevice, RechargeConfig,
    RechargeOrder, RoomId,
};
