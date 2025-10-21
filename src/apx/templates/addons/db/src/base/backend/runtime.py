from sqlalchemy import Engine
from .config import conf, AppConfig
from databricks.sdk import WorkspaceClient
from sqlmodel import Session
from sqlalchemy import create_engine, event


class Runtime:
    def __init__(self):
        self.config: AppConfig = conf
        # check if the database instance exists
        if not self.ws.database.get_database_instance(self.config.db.instance_name):
            raise ValueError(
                f"Database instance {self.config.db.instance_name} does not exist"
            )

        # check if a connection to the database can be established
        try:
            self.engine.connect()
        except Exception as e:
            raise ConnectionError(f"Failed to connect to the database: {e}")

    @property
    def ws(self) -> WorkspaceClient:
        # note - this workspace client is usually an SP-based client
        # in development it usually uses the DATABRICKS_CONFIG_PROFILE
        return WorkspaceClient()

    @property
    def engine_url(self) -> str:
        instance = self.ws.database.get_database_instance(self.config.db.instance_name)
        # f"postgresql+psycopg://{postgres_username}:@{postgres_host}:{postgres_port}/{postgres_database}"
        prefix = "postgresql+psycopg"
        host = instance.read_write_dns
        port = self.config.db.port
        database = self.config.db.database_name
        username = (
            self.ws.config.client_id
            if self.ws.config.client_id
            else self.ws.current_user.me().user_name
        )
        return f"{prefix}://{username}:@{host}:{port}/{database}"

    def _before_connect(self, dialect, conn_rec, cargs, cparams):
        cparams["password"] = self.ws.config.oauth_token().access_token

    @property
    def engine(self) -> Engine:
        engine = create_engine(self.engine_url, pool_recycle=45 * 60)  # 45 minutes
        event.listens_for(engine, "do_connect")(self._before_connect)
        return engine

    def get_session(self) -> Session:
        return Session(self.engine)


rt = Runtime()
