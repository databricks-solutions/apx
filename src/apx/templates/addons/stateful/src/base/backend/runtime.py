from databricks.sdk.errors import NotFound
from sqlalchemy import Engine
from .config import conf, AppConfig
from databricks.sdk import WorkspaceClient
from sqlmodel import SQLModel, Session, text
from sqlalchemy import create_engine, event
from .logger import logger


class Runtime:
    def __init__(self):
        self.config: AppConfig = conf

    @property
    def ws(self) -> WorkspaceClient:
        # note - this workspace client is usually an SP-based client
        # in development it usually uses the DATABRICKS_CONFIG_PROFILE
        return WorkspaceClient()

    @property
    def engine_url(self) -> str:
        instance = self.ws.database.get_database_instance(self.config.db.instance_name)
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

    def validate_db(self) -> None:
        logger.info(
            f"Validating database connection to instance {self.config.db.instance_name}"
        )
        # check if the database instance exists
        try:
            self.ws.database.get_database_instance(self.config.db.instance_name)
        except NotFound:
            raise ValueError(
                f"Database instance {self.config.db.instance_name} does not exist"
            )

        # check if a connection to the database can be established
        try:
            with self.get_session() as session:
                session.connection().execute(text("SELECT 1"))
                session.close()

        except Exception:
            raise ConnectionError("Failed to connect to the database")

        logger.info(
            f"Database connection to instance {self.config.db.instance_name} validated successfully"
        )

    def initialize_models(self) -> None:
        logger.info("Initializing database models")
        SQLModel.metadata.create_all(self.engine)
        logger.info("Database models initialized successfully")


rt = Runtime()
